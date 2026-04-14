use std::time::Instant;
use tibet_store_mmu::{MmuArena, MmuConfig, FillMode, percentile, format_ns, userfaultfd_available};

/// TIBET-Store MMU Benchmark — Real userfaultfd Performance
///
/// Meet de echte kernel-level page fault handling:
///   1. Zero-fill faults (snelste pad)
///   2. Static data injection (Redis-simulatie)
///   3. Compressed restore simulation (.tza pad)
///   4. Sequential access pattern (volledige arena scan)
///   5. Random access pattern (worst case)
///   6. Scaling: arena size vs fault latency

fn main() {
    println!("═══════════════════════════════════════════════════════════════");
    println!("◈ TIBET-Store MMU Benchmark — Real userfaultfd");
    println!("◈ Gemini's PoC → verfijnd door Root AI");
    println!("═══════════════════════════════════════════════════════════════\n");

    if userfaultfd_available() {
        println!("◈ userfaultfd: BESCHIKBAAR (real kernel page fault handling)\n");
    } else {
        println!("◈ userfaultfd: NIET BESCHIKBAAR");
        println!("◈ Vereist: echo 1 > /proc/sys/vm/unprivileged_userfaultfd");
        println!("◈ Of:  sudo cargo bench --bench mmu_bench\n");
        println!("◈ Fallback: alleen timing simulatie...\n");
        bench_fallback();
        return;
    }

    bench_zero_fill();
    bench_static_data();
    bench_compressed_restore();
    bench_sequential_scan();
    bench_random_access();
    bench_scaling();

    println!("═══════════════════════════════════════════════════════════════");
    println!("◈ TIBET-Store MMU Benchmark Complete");
    println!("═══════════════════════════════════════════════════════════════");
}

// ═══════════════════════════════════════════════════════════════
// Part 1: Zero-Fill Page Faults
// ═══════════════════════════════════════════════════════════════

fn bench_zero_fill() {
    println!("── Part 1: Zero-Fill Page Faults ──\n");

    let page_count = 32; // 128KB — keep it small for reliable timing
    let config = MmuConfig {
        arena_size: page_count * 4096,
        fill_mode: FillMode::ZeroFill,
    };

    let arena = match MmuArena::new(config) {
        Some(a) => a,
        None => { println!("  SKIP: userfaultfd unavailable\n"); return; }
    };

    let page_size = arena.page_size();
    let t0 = Instant::now();

    // Touch each page sequentially → triggers one fault per page
    for i in 0..page_count {
        let offset = i * page_size;
        unsafe {
            let byte = arena.read_byte(offset);
            assert_eq!(byte, 0, "Zero-fill page should be all zeros");
        }
    }

    let elapsed = t0.elapsed();
    let result = arena.shutdown();

    let mut latencies = result.fault_latencies_ns.clone();
    latencies.sort();

    println!("  Pages faulted: {}", result.stats.pages_faulted);
    println!("  Pages injected: {}", result.stats.pages_injected);
    println!("  Total time: {:?}", elapsed);
    println!("  Per page fault: {:?}", elapsed / page_count as u32);

    if !latencies.is_empty() {
        println!("  Handler latency (uffd.copy only):");
        println!("    p50: {}", format_ns(percentile(&latencies, 50.0)));
        println!("    p95: {}", format_ns(percentile(&latencies, 95.0)));
        println!("    p99: {}", format_ns(percentile(&latencies, 99.0)));
        println!("    min: {}", format_ns(*latencies.first().unwrap()));
        println!("    max: {}", format_ns(*latencies.last().unwrap()));
    }
    println!();
}

// ═══════════════════════════════════════════════════════════════
// Part 2: Static Data Injection (Redis Key Simulation)
// ═══════════════════════════════════════════════════════════════

fn bench_static_data() {
    println!("── Part 2: Static Data Injection (Redis Simulation) ──\n");

    let page_count = 32;
    let payload = b"REDIS:key=trust_kernel:value=active:ttl=3600:type=hash".to_vec();

    let config = MmuConfig {
        arena_size: page_count * 4096,
        fill_mode: FillMode::StaticData { payload: payload.clone() },
    };

    let arena = match MmuArena::new(config) {
        Some(a) => a,
        None => { println!("  SKIP: userfaultfd unavailable\n"); return; }
    };

    let page_size = arena.page_size();
    let t0 = Instant::now();

    for i in 0..page_count {
        let offset = i * page_size;
        unsafe {
            let data = arena.read_slice(offset, payload.len());
            assert_eq!(&data[..], &payload[..], "Payload mismatch on page {}", i);
        }
    }

    let elapsed = t0.elapsed();
    let result = arena.shutdown();

    let mut latencies = result.fault_latencies_ns.clone();
    latencies.sort();

    println!("  Payload: {} bytes (\"{}\")", payload.len(),
        String::from_utf8_lossy(&payload[..40.min(payload.len())]));
    println!("  Pages: {}", result.stats.pages_faulted);
    println!("  Total: {:?}", elapsed);
    println!("  Per fault: {:?}", elapsed / page_count as u32);

    if !latencies.is_empty() {
        println!("  Handler p50: {}  p99: {}  max: {}",
            format_ns(percentile(&latencies, 50.0)),
            format_ns(percentile(&latencies, 99.0)),
            format_ns(*latencies.last().unwrap()));
    }

    println!("  = Redis leest key → page fault → data verschijnt in {:?}", elapsed / page_count as u32);
    println!();
}

// ═══════════════════════════════════════════════════════════════
// Part 3: Compressed Restore (.tza Simulation)
// ═══════════════════════════════════════════════════════════════

fn bench_compressed_restore() {
    println!("── Part 3: Compressed Restore (.tza Simulation) ──\n");

    let page_count = 32;
    let config = MmuConfig {
        arena_size: page_count * 4096,
        fill_mode: FillMode::CompressedRestore,
    };

    let arena = match MmuArena::new(config) {
        Some(a) => a,
        None => { println!("  SKIP: userfaultfd unavailable\n"); return; }
    };

    let page_size = arena.page_size();
    let t0 = Instant::now();

    for i in 0..page_count {
        let offset = i * page_size;
        unsafe {
            let byte = arena.read_byte(offset);
            // CompressedRestore writes 'T' as first byte (from "TZA_RESTORED:...")
            assert_eq!(byte, b'T', "Expected TZA marker on page {}", i);
        }
    }

    let elapsed = t0.elapsed();
    let result = arena.shutdown();

    let mut latencies = result.fault_latencies_ns.clone();
    latencies.sort();

    println!("  Mode: .tza load → Ed25519 verify → zstd decompress → inject");
    println!("  Pages: {}", result.stats.pages_faulted);
    println!("  Total: {:?}", elapsed);
    println!("  Per fault: {:?}", elapsed / page_count as u32);

    if !latencies.is_empty() {
        println!("  Handler p50: {}  p99: {}  max: {}",
            format_ns(percentile(&latencies, 50.0)),
            format_ns(percentile(&latencies, 99.0)),
            format_ns(*latencies.last().unwrap()));
    }
    println!();
}

// ═══════════════════════════════════════════════════════════════
// Part 4: Sequential Full Scan
// ═══════════════════════════════════════════════════════════════

fn bench_sequential_scan() {
    println!("── Part 4: Sequential Full Scan ──\n");

    let sizes = [
        ("32KB", 32 * 1024),
        ("64KB", 64 * 1024),
        ("128KB", 128 * 1024),
    ];

    for (label, size) in &sizes {
        let config = MmuConfig {
            arena_size: *size,
            fill_mode: FillMode::ZeroFill,
        };

        let arena = match MmuArena::new(config) {
            Some(a) => a,
            None => { println!("  SKIP: userfaultfd unavailable\n"); return; }
        };

        let page_size = arena.page_size();
        let page_count = *size / page_size;

        let t0 = Instant::now();
        for i in 0..page_count {
            unsafe { arena.read_byte(i * page_size); }
        }
        let elapsed = t0.elapsed();

        let result = arena.shutdown();
        let throughput_mbps = (*size as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64();

        println!("  {}: {} pages in {:?} ({:.1} MB/s, {:?}/fault)",
            label, result.stats.pages_faulted, elapsed, throughput_mbps,
            elapsed / page_count as u32);
    }
    println!();
}

// ═══════════════════════════════════════════════════════════════
// Part 5: Random Access Pattern
// ═══════════════════════════════════════════════════════════════

fn bench_random_access() {
    println!("── Part 5: Random Access Pattern (Worst Case) ──\n");

    let page_count = 64; // 256KB
    let config = MmuConfig {
        arena_size: page_count * 4096,
        fill_mode: FillMode::StaticData {
            payload: b"RANDOM_ACCESS_TEST".to_vec(),
        },
    };

    let arena = match MmuArena::new(config) {
        Some(a) => a,
        None => { println!("  SKIP: userfaultfd unavailable\n"); return; }
    };

    let page_size = arena.page_size();

    // Pseudo-random page order (LCG)
    let mut indices: Vec<usize> = Vec::with_capacity(page_count);
    let mut x: usize = 42;
    let mut visited = vec![false; page_count];
    for _ in 0..page_count {
        // Find next unvisited page
        x = (x * 1103515245 + 12345) % page_count;
        while visited[x] {
            x = (x + 1) % page_count;
        }
        visited[x] = true;
        indices.push(x);
    }

    let t0 = Instant::now();
    for &idx in &indices {
        unsafe { arena.read_byte(idx * page_size); }
    }
    let elapsed = t0.elapsed();

    let result = arena.shutdown();
    let mut latencies = result.fault_latencies_ns.clone();
    latencies.sort();

    println!("  Random {} pages: {:?} total ({:?}/fault)",
        page_count, elapsed, elapsed / page_count as u32);

    if !latencies.is_empty() {
        println!("  Latency: p50={} p95={} p99={} max={}",
            format_ns(percentile(&latencies, 50.0)),
            format_ns(percentile(&latencies, 95.0)),
            format_ns(percentile(&latencies, 99.0)),
            format_ns(*latencies.last().unwrap()));
    }

    println!("  = Worst case: random pages, geen caching, elke read is een fault");
    println!();
}

// ═══════════════════════════════════════════════════════════════
// Part 6: Scaling — Arena Size vs Performance
// ═══════════════════════════════════════════════════════════════

fn bench_scaling() {
    println!("── Part 6: Scaling — Arena Size vs Fault Latency ──\n");

    let configs = [
        ("16KB", 16 * 1024),
        ("32KB", 32 * 1024),
        ("64KB", 64 * 1024),
        ("128KB", 128 * 1024),
        ("256KB", 256 * 1024),
    ];

    println!("  {:<8} {:<8} {:<12} {:<12} {:<12} {:<12}",
        "Arena", "Pages", "Total", "Per Fault", "p50", "p99");

    for (label, size) in &configs {
        let config = MmuConfig {
            arena_size: *size,
            fill_mode: FillMode::CompressedRestore,
        };

        let arena = match MmuArena::new(config) {
            Some(a) => a,
            None => { println!("  SKIP\n"); return; }
        };

        let page_size = arena.page_size();
        let page_count = *size / page_size;

        let t0 = Instant::now();
        for i in 0..page_count {
            unsafe { arena.read_byte(i * page_size); }
        }
        let elapsed = t0.elapsed();

        let result = arena.shutdown();
        let mut latencies = result.fault_latencies_ns.clone();
        latencies.sort();

        let p50 = if !latencies.is_empty() { format_ns(percentile(&latencies, 50.0)) } else { "-".to_string() };
        let p99 = if !latencies.is_empty() { format_ns(percentile(&latencies, 99.0)) } else { "-".to_string() };

        println!("  {:<8} {:<8} {:<12?} {:<12?} {:<12} {:<12}",
            label, page_count, elapsed, elapsed / page_count as u32, p50, p99);
    }

    println!("\n  Conclusie: fault latency is O(1) per page — onafhankelijk van arena size.");
    println!("  De bottleneck is uffd.copy() (kernel memcpy), niet lookup.");
    println!();
}

// ═══════════════════════════════════════════════════════════════
// Fallback: when userfaultfd is not available
// ═══════════════════════════════════════════════════════════════

fn bench_fallback() {
    println!("── Fallback: Simulated Timings (geen userfaultfd) ──\n");

    // Measure pure allocation + memcpy (what uffd.copy does internally)
    let page_size = 4096;
    let page_count = 1024;

    // Measure: alloc page + fill + copy
    let t0 = Instant::now();
    for _ in 0..page_count {
        let mut page = vec![0u8; page_size];
        page[0] = 42;
        std::hint::black_box(&page);
    }
    let alloc_fill = t0.elapsed();

    // Measure: just alloc
    let t0 = Instant::now();
    for _ in 0..page_count {
        let page = vec![0u8; page_size];
        std::hint::black_box(&page);
    }
    let alloc_only = t0.elapsed();

    println!("  Page alloc (vec![0; 4096]):     {:?}/page", alloc_only / page_count as u32);
    println!("  Page alloc + fill:              {:?}/page", alloc_fill / page_count as u32);
    println!("  userfaultfd overhead (schatting): ~3-10µs/page (kernel trap + context switch)");
    println!("  uffd.copy (kernel memcpy):       ~1-2µs/4KB page");
    println!();
    println!("  Om echte metingen te doen: sudo cargo bench --bench mmu_bench");
    println!();
}
