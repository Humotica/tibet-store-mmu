use libc::{mmap, munmap, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE, sysconf, _SC_PAGESIZE};
use std::ptr;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;
use userfaultfd::{Uffd, UffdBuilder, Event};

/// TIBET-Store MMU — Transparante Geheugen-Virtualisatie
///
/// Gemini's bewezen concept, verfijnd tot een meetbare library:
///   - mmap: virtueel RAM zonder fysieke backing
///   - userfaultfd: page fault trap zonder SIGSEGV
///   - Archivaris thread: fault → fetch .tza → decompress → inject → resume
///
/// Drie modi:
///   - ZeroFill: inject zero page (snelst, voor virgin memory)
///   - StaticData: inject vaste payload (Redis-simulatie)
///   - CompressedRestore: simuleer .tza decompress + inject (productie-pad)

// ═══════════════════════════════════════════════════════════════
// Public Types
// ═══════════════════════════════════════════════════════════════

/// Configuration for the MMU illusion.
#[derive(Debug, Clone)]
pub struct MmuConfig {
    /// Total virtual arena size in bytes (must be page-aligned)
    pub arena_size: usize,
    /// What to inject on page fault
    pub fill_mode: FillMode,
}

/// What to inject when a page fault occurs.
#[derive(Debug, Clone)]
pub enum FillMode {
    /// Zero page (fastest — no data copy needed)
    ZeroFill,
    /// Static payload (copy same data into every page)
    StaticData { payload: Vec<u8> },
    /// Simulated .tza restore (zstd decompress simulation)
    CompressedRestore,
}

/// Stats from the MMU handler.
#[derive(Debug, Clone)]
pub struct MmuStats {
    pub pages_faulted: u64,
    pub pages_injected: u64,
    pub inject_errors: u64,
    pub total_bytes_injected: u64,
    pub page_size: usize,
    pub arena_size: usize,
    pub arena_pages: usize,
}

/// Result of running the MMU illusion.
#[derive(Debug)]
pub struct MmuResult {
    pub stats: MmuStats,
    pub elapsed: std::time::Duration,
    pub fault_latencies_ns: Vec<u64>,
}

// ═══════════════════════════════════════════════════════════════
// MMU Arena — The core abstraction
// ═══════════════════════════════════════════════════════════════

/// An MMU-trapped virtual memory arena.
///
/// The arena is a region of virtual memory with no physical backing.
/// When any thread touches an address in the arena, userfaultfd catches
/// the page fault and the handler thread injects the requested data.
pub struct MmuArena {
    /// Base address of the mmap'd region
    addr: *mut libc::c_void,
    /// Arena size in bytes
    size: usize,
    /// System page size
    page_size: usize,
    /// Stats counters
    pages_faulted: Arc<AtomicU64>,
    pages_injected: Arc<AtomicU64>,
    inject_errors: Arc<AtomicU64>,
    bytes_injected: Arc<AtomicU64>,
    /// Handler thread alive flag
    handler_active: Arc<AtomicBool>,
    /// Handler thread join handle
    handler_thread: Option<JoinHandle<Vec<u64>>>,
}

impl MmuArena {
    /// Create a new MMU arena.
    ///
    /// This:
    ///   1. Gets the system page size
    ///   2. mmap's a virtual region (MAP_ANONYMOUS, no physical backing)
    ///   3. Creates a userfaultfd and registers the region
    ///   4. Spawns a handler thread that listens for page faults
    ///
    /// Returns None if userfaultfd is not available (needs root or CAP_SYS_PTRACE).
    pub fn new(config: MmuConfig) -> Option<Self> {
        let page_size = unsafe { sysconf(_SC_PAGESIZE) as usize };

        // Align arena size to page boundary
        let size = (config.arena_size + page_size - 1) & !(page_size - 1);

        // Step 1: Allocate virtual memory (no physical backing)
        let addr = unsafe {
            mmap(
                ptr::null_mut(),
                size,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if addr == libc::MAP_FAILED {
            return None;
        }

        // Step 2: Create userfaultfd (non-blocking so handler can check active flag)
        let uffd = match UffdBuilder::new()
            .close_on_exec(true)
            .non_blocking(true)
            .user_mode_only(true)
            .create()
        {
            Ok(u) => u,
            Err(_) => {
                // Clean up mmap
                unsafe { munmap(addr, size); }
                return None;
            }
        };

        // Step 3: Register the arena
        if uffd.register(addr, size).is_err() {
            unsafe { munmap(addr, size); }
            return None;
        }

        // Step 4: Spawn handler thread
        let pages_faulted = Arc::new(AtomicU64::new(0));
        let pages_injected = Arc::new(AtomicU64::new(0));
        let inject_errors = Arc::new(AtomicU64::new(0));
        let bytes_injected = Arc::new(AtomicU64::new(0));
        let handler_active = Arc::new(AtomicBool::new(true));

        let pf = pages_faulted.clone();
        let pi = pages_injected.clone();
        let ie = inject_errors.clone();
        let bi = bytes_injected.clone();
        let ha = handler_active.clone();
        let handler_ready = Arc::new(AtomicBool::new(false));
        let hr = handler_ready.clone();
        let fill_mode = config.fill_mode;

        let handler_thread = thread::spawn(move || {
            let mut latencies: Vec<u64> = Vec::new();

            // Signal that handler is ready to receive faults
            hr.store(true, Ordering::Release);

            loop {
                if !ha.load(Ordering::Relaxed) {
                    break;
                }

                match uffd.read_event() {
                    Ok(None) => {
                        // Non-blocking: no event yet, brief yield and retry
                        thread::yield_now();
                        continue;
                    }
                    Err(_) => {
                        // Non-blocking: nothing pending or uffd error, brief yield
                        thread::yield_now();
                        continue;
                    }
                    Ok(Some(Event::Pagefault { addr: fault_addr, .. })) => {
                        let t0 = Instant::now();
                        let aligned = (fault_addr as usize / page_size) * page_size;

                        pf.fetch_add(1, Ordering::Relaxed);

                        // Build injection data based on fill mode
                        let data = match &fill_mode {
                            FillMode::ZeroFill => {
                                vec![0u8; page_size]
                            }
                            FillMode::StaticData { payload } => {
                                let mut page = vec![0u8; page_size];
                                let copy_len = payload.len().min(page_size);
                                page[..copy_len].copy_from_slice(&payload[..copy_len]);
                                page
                            }
                            FillMode::CompressedRestore => {
                                // Simulate .tza restore:
                                //   1. "Load" compressed block (~page_size/4 bytes)
                                //   2. "Decompress" with zstd (~1ns/byte output)
                                //   3. "Verify" Ed25519 signature
                                //   4. Build output page
                                let mut page = vec![0u8; page_size];
                                // Write a marker so we can verify the restore worked
                                let marker = format!("TZA_RESTORED:page@{:#x}", aligned);
                                let marker_bytes = marker.as_bytes();
                                page[..marker_bytes.len()].copy_from_slice(marker_bytes);
                                page
                            }
                        };

                        // THE MAGIC: inject data into the faulting page
                        let result = unsafe {
                            uffd.copy(
                                data.as_ptr() as *const _,
                                aligned as *mut _,
                                page_size,
                                true, // wake the blocked thread
                            )
                        };

                        match result {
                            Ok(_) => {
                                pi.fetch_add(1, Ordering::Relaxed);
                                bi.fetch_add(page_size as u64, Ordering::Relaxed);
                            }
                            Err(_) => {
                                ie.fetch_add(1, Ordering::Relaxed);
                            }
                        }

                        let latency_ns = t0.elapsed().as_nanos() as u64;
                        latencies.push(latency_ns);
                    }
                    Ok(None) => {
                        // No event (shouldn't happen in blocking mode)
                        break;
                    }
                    Ok(Some(_)) => {
                        // Other event (fork, remap, etc.) — ignore
                    }
                    Err(_) => {
                        // UFFD closed or error
                        break;
                    }
                }
            }

            latencies
        });

        // Wait for handler thread to be ready before returning
        while !handler_ready.load(Ordering::Acquire) {
            thread::yield_now();
        }
        // Give handler time to enter read_event() blocking call
        // Without this, the first page fault can arrive before the handler is listening
        thread::sleep(std::time::Duration::from_millis(5));

        Some(Self {
            addr,
            size,
            page_size,
            pages_faulted,
            pages_injected,
            inject_errors,
            bytes_injected,
            handler_active,
            handler_thread: Some(handler_thread),
        })
    }

    /// Get the base address of the arena (for reading/writing).
    pub fn addr(&self) -> *mut libc::c_void {
        self.addr
    }

    /// Get the arena size.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Get the system page size.
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// Number of pages in the arena.
    pub fn page_count(&self) -> usize {
        self.size / self.page_size
    }

    /// Read a byte from offset (will trigger page fault if page not yet loaded).
    ///
    /// # Safety
    /// Offset must be within arena bounds.
    pub unsafe fn read_byte(&self, offset: usize) -> u8 {
        let ptr = (self.addr as *const u8).add(offset);
        ptr::read_volatile(ptr)
    }

    /// Read a slice from offset (may trigger multiple page faults).
    ///
    /// # Safety
    /// Range must be within arena bounds.
    pub unsafe fn read_slice(&self, offset: usize, len: usize) -> Vec<u8> {
        let ptr = (self.addr as *const u8).add(offset);
        let slice = std::slice::from_raw_parts(ptr, len);
        slice.to_vec()
    }

    /// Get current stats (non-blocking).
    pub fn stats(&self) -> MmuStats {
        MmuStats {
            pages_faulted: self.pages_faulted.load(Ordering::Relaxed),
            pages_injected: self.pages_injected.load(Ordering::Relaxed),
            inject_errors: self.inject_errors.load(Ordering::Relaxed),
            total_bytes_injected: self.bytes_injected.load(Ordering::Relaxed),
            page_size: self.page_size,
            arena_size: self.size,
            arena_pages: self.size / self.page_size,
        }
    }

    /// Shut down the handler and collect latency data.
    pub fn shutdown(mut self) -> MmuResult {
        let t0 = Instant::now();
        // Signal handler to stop (non-blocking handler checks this flag)
        self.handler_active.store(false, Ordering::Release);

        let latencies = if let Some(handle) = self.handler_thread.take() {
            handle.join().unwrap_or_default()
        } else {
            Vec::new()
        };

        // Clean up mmap
        if !self.addr.is_null() {
            unsafe { munmap(self.addr, self.size); }
            self.addr = ptr::null_mut();
        }

        let stats = MmuStats {
            pages_faulted: self.pages_faulted.load(Ordering::Relaxed),
            pages_injected: self.pages_injected.load(Ordering::Relaxed),
            inject_errors: self.inject_errors.load(Ordering::Relaxed),
            total_bytes_injected: self.bytes_injected.load(Ordering::Relaxed),
            page_size: self.page_size,
            arena_size: self.size,
            arena_pages: self.size / self.page_size,
        };

        MmuResult {
            stats,
            elapsed: t0.elapsed(),
            fault_latencies_ns: latencies,
        }
    }
}

impl Drop for MmuArena {
    fn drop(&mut self) {
        self.handler_active.store(false, Ordering::Release);
        if !self.addr.is_null() {
            unsafe { munmap(self.addr, self.size); }
        }
        // Note: handler thread will exit when uffd read fails after munmap
    }
}

// ═══════════════════════════════════════════════════════════════
// Helper: compute percentiles from sorted latency data
// ═══════════════════════════════════════════════════════════════

pub fn percentile(sorted: &[u64], pct: f64) -> u64 {
    if sorted.is_empty() { return 0; }
    let idx = ((sorted.len() as f64 * pct / 100.0) as usize).min(sorted.len() - 1);
    sorted[idx]
}

/// Quick check: is userfaultfd available on this system?
pub fn userfaultfd_available() -> bool {
    match UffdBuilder::new()
        .close_on_exec(true)
        .non_blocking(true)
        .user_mode_only(true)
        .create()
    {
        Ok(_) => true,
        Err(_) => false,
    }
}

pub fn format_ns(ns: u64) -> String {
    if ns < 1_000 {
        format!("{}ns", ns)
    } else if ns < 1_000_000 {
        format!("{:.1}µs", ns as f64 / 1_000.0)
    } else {
        format!("{:.2}ms", ns as f64 / 1_000_000.0)
    }
}
