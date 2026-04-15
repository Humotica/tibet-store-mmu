use std::time::Instant;
use tibet_store_mmu::*;
use tibet_trust_kernel::bifurcation::ClearanceLevel;

fn main() {
    println!();
    println!("  ╔═══════════════════════════════════════════════════════════╗");
    println!("  ║  SPACESHUTTLE — Encrypted Memory Illusion                ║");
    println!("  ║                                                          ║");
    println!("  ║  Page fault → AES-256-GCM decrypt → inject → resume     ║");
    println!("  ║  Geen JIS claim = dood materiaal.                        ║");
    println!("  ║  Identity IS the memory.                                 ║");
    println!("  ╚═══════════════════════════════════════════════════════════╝");
    println!();

    // ── Step 0: System check ──
    if !userfaultfd_available() {
        println!("  ✗ userfaultfd niet beschikbaar (root of CAP_SYS_PTRACE nodig)");
        println!("  Tip: sudo sysctl -w vm.unprivileged_userfaultfd=1");
        return;
    }
    println!("  ✓ userfaultfd beschikbaar");

    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
    let num_pages = 64;
    let arena_size = page_size * num_pages;

    println!("  ✓ Page size: {} bytes", page_size);
    println!("  ✓ Arena: {} pages = {} KB", num_pages, arena_size / 1024);
    println!();

    // ── Step 1: Prepare secret data (simulate Redis/LLM state) ──
    println!("  ── Fase 1: Sealing {} pages met AES-256-GCM ──", num_pages);
    let t0 = Instant::now();

    let pages: Vec<Vec<u8>> = (0..num_pages)
        .map(|i| {
            let mut page = vec![0u8; page_size];
            let secret = format!(
                "PAGE[{:04}] Secret data — encrypted at rest, decrypted on fault. \
                 Only TopSecret clearance can read this. #HashesFromHolland",
                i
            );
            let bytes = secret.as_bytes();
            page[..bytes.len()].copy_from_slice(bytes);
            page
        })
        .collect();

    let sealed = seal_pages(&pages, ClearanceLevel::Secret, "spaceshuttle");
    let seal_us = t0.elapsed().as_micros();
    let seal_per = seal_us / num_pages as u128;

    println!("  Sealed:   {} pages in {} us ({} us/page)", num_pages, seal_us, seal_per);
    println!();

    // ── Step 2: Create arena with encrypted fill mode ──
    println!("  ── Fase 2: MMU Arena — {} KB virtueel, 0 bytes fysiek ──", arena_size / 1024);

    // TopSecret claim — should be able to read Secret data
    let claim = mmu_claim("spaceshuttle.aint", ClearanceLevel::TopSecret);

    let arena = MmuArena::new(MmuConfig {
        arena_size,
        fill_mode: FillMode::EncryptedRestore {
            sealed_pages: sealed.clone(),
            claim: claim.clone(),
            clearance: ClearanceLevel::Secret,
        },
    });

    let arena = match arena {
        Some(a) => a,
        None => {
            println!("  ✗ Arena creatie mislukt");
            return;
        }
    };

    println!("  ✓ Arena actief op {:?}", arena.addr());
    println!("  ✓ Handler thread luistert op page faults");
    println!();

    // ── Step 3: Touch pages — trigger encrypted page faults ──
    println!("  ── Fase 3: Page faults triggeren (decrypt-on-read) ──");
    let t0 = Instant::now();

    let mut verified = 0usize;
    let mut failed = 0usize;

    for i in 0..num_pages {
        let offset = i * page_size;
        let read_data = unsafe { arena.read_slice(offset, 80) };
        let text = String::from_utf8_lossy(&read_data);

        if text.starts_with(&format!("PAGE[{:04}]", i)) {
            verified += 1;
        } else {
            failed += 1;
            if failed <= 3 {
                println!("  ✗ Page {} mismatch: {:?}", i, &text[..40.min(text.len())]);
            }
        }
    }

    let read_us = t0.elapsed().as_micros();
    let read_per = read_us / num_pages as u128;
    let stats = arena.stats();

    println!();
    println!("  Decrypted: {}/{} pages verified", verified, num_pages);
    println!("  Faults:    {} triggered, {} injected, {} errors",
        stats.pages_faulted, stats.pages_injected, stats.inject_errors);
    println!("  Latency:   {} us total, {} us/page (decrypt + inject)", read_us, read_per);
    println!();

    // ── Step 4: Access denied test — Restricted can't read Secret ──
    println!("  ── Fase 4: Access Control — Restricted → Secret ──");

    let low_claim = mmu_claim("intruder.aint", ClearanceLevel::Restricted);

    let arena_denied = MmuArena::new(MmuConfig {
        arena_size: page_size * 4, // Just 4 pages
        fill_mode: FillMode::EncryptedRestore {
            sealed_pages: sealed[..4].to_vec(),
            claim: low_claim,
            clearance: ClearanceLevel::Secret,
        },
    });

    if let Some(denied_arena) = arena_denied {
        let read_data = unsafe { denied_arena.read_slice(0, 80) };
        let all_zero = read_data.iter().all(|&b| b == 0);

        if all_zero {
            println!("  ✓ DENIED — Restricted claim → zero page (dood materiaal)");
        } else {
            println!("  ✗ FOUT — Restricted claim kon Secret data lezen!");
        }

        let denied_stats = denied_arena.stats();
        println!("  Faults: {}, Injected: {} (zero pages)",
            denied_stats.pages_faulted, denied_stats.pages_injected);

        let _ = denied_arena.shutdown();
    }

    // ── Step 5: Shutdown encrypt-only arena ──
    let result_enc = arena.shutdown();
    let mut sorted_enc = result_enc.fault_latencies_ns.clone();
    sorted_enc.sort();

    println!();

    // ══════════════════════════════════════════════════════════════
    // FASE 5: COMPRESSED + ENCRYPTED — de productie-modus
    // plaintext → zstd → AES-256-GCM → stored
    // fault → open → zstd decompress → inject
    // ══════════════════════════════════════════════════════════════

    println!("  ╔═══════════════════════════════════════════════════════════╗");
    println!("  ║  FASE 5: Compressed + Encrypted (Productie-modus)        ║");
    println!("  ║  plaintext → zstd → AES-256-GCM → fault → decompress    ║");
    println!("  ╚═══════════════════════════════════════════════════════════╝");
    println!();

    // Seal compressed
    let t0 = Instant::now();
    let comp_result = seal_pages_compressed(
        &pages,
        ClearanceLevel::Secret,
        "spaceshuttle-v2",
        3, // zstd level 3
    );
    let comp_seal_us = t0.elapsed().as_micros();
    let comp_seal_per = comp_seal_us / num_pages as u128;

    println!("  Compressed + Sealed: {} pages in {} us ({} us/page)", num_pages, comp_seal_us, comp_seal_per);
    println!("  Original:    {:>7} bytes ({} KB)", comp_result.total_original, comp_result.total_original / 1024);
    println!("  Compressed:  {:>7} bytes ({:.1}x ratio)", comp_result.total_compressed, comp_result.compression_ratio);
    println!("  Encrypted:   {:>7} bytes (ciphertext)", comp_result.total_encrypted);
    println!("  Besparing:   {:.0}% minder opslag",
        (1.0 - comp_result.total_encrypted as f64 / comp_result.total_original as f64) * 100.0);
    println!();

    // Create compressed+encrypted arena
    let comp_claim = mmu_claim("spaceshuttle-v2.aint", ClearanceLevel::TopSecret);

    let comp_arena = MmuArena::new(MmuConfig {
        arena_size,
        fill_mode: FillMode::CompressedEncryptedRestore {
            sealed_pages: comp_result.blocks,
            original_sizes: comp_result.original_sizes,
            claim: comp_claim,
            clearance: ClearanceLevel::Secret,
        },
    }).expect("Compressed arena failed");

    // Touch all pages — decrypt + decompress on fault
    let t0 = Instant::now();
    let mut comp_verified = 0usize;

    for i in 0..num_pages {
        let offset = i * page_size;
        let read_data = unsafe { comp_arena.read_slice(offset, 80) };
        let text = String::from_utf8_lossy(&read_data);
        if text.starts_with(&format!("PAGE[{:04}]", i)) {
            comp_verified += 1;
        }
    }

    let comp_read_us = t0.elapsed().as_micros();
    let comp_read_per = comp_read_us / num_pages as u128;
    let comp_stats = comp_arena.stats();

    println!("  Decrypted + Decompressed: {}/{} pages verified", comp_verified, num_pages);
    println!("  Faults:    {} triggered, {} injected", comp_stats.pages_faulted, comp_stats.pages_injected);
    println!("  Latency:   {} us total, {} us/page (open + zstd + inject)", comp_read_us, comp_read_per);

    let result_comp = comp_arena.shutdown();
    let mut sorted_comp = result_comp.fault_latencies_ns.clone();
    sorted_comp.sort();

    println!();

    // ── Final comparison ──
    println!("  ══════════════════════════════════════════════════════════════");
    println!("  VERGELIJKING — Encrypt-only vs Compressed+Encrypted");
    println!("  ──────────────────────────────────────────────────────────────");
    println!("                      Encrypt-only    Compress+Encrypt");
    println!("  Seal:              {:>6} us/pg     {:>6} us/pg", seal_per, comp_seal_per);
    println!("  Fault+Decrypt:     {:>6} us/pg     {:>6} us/pg", read_per, comp_read_per);
    println!("  Storage:           {:>6} KB        {:>6} KB",
        num_pages as usize * page_size / 1024,
        comp_result.total_encrypted / 1024);
    if !sorted_enc.is_empty() && !sorted_comp.is_empty() {
        println!("  p50:               {:>10}     {:>10}",
            format_ns(percentile(&sorted_enc, 50.0)),
            format_ns(percentile(&sorted_comp, 50.0)));
        println!("  p95:               {:>10}     {:>10}",
            format_ns(percentile(&sorted_enc, 95.0)),
            format_ns(percentile(&sorted_comp, 95.0)));
        println!("  p99:               {:>10}     {:>10}",
            format_ns(percentile(&sorted_enc, 99.0)),
            format_ns(percentile(&sorted_comp, 99.0)));
    }
    println!("  Compression:       {:>10}     {:>8.1}x",
        "1.0x (none)", comp_result.compression_ratio);
    println!("  ──────────────────────────────────────────────────────────────");
    println!();
    println!("  ═══════════════════════════════════════════════════════════");
    println!("  SPACESHUTTLE v2 — Compressed Encrypted Memory Illusion");
    println!();
    println!("  Pipeline:  plaintext → zstd → AES-256-GCM → stored");
    println!("  Fault:     open → decompress → inject → resume");
    println!("  Crypto:    AES-256-GCM + X25519 + HKDF-SHA256");
    println!("  MMU:       userfaultfd (kernel-level page trap)");
    println!("  Compress:  zstd level 3 ({:.1}x ratio)", comp_result.compression_ratio);
    println!();
    println!("  Kleiner EN veiliger. Encryptie maakt het niet langzamer,");
    println!("  het maakt het kleiner.");
    println!();
    println!("  Geen JIS claim = dood materiaal.");
    println!("  Identity IS the memory.");
    println!();
    println!("  #HashesFromHolland #Spaceshuttle");
    println!("  ═══════════════════════════════════════════════════════════");
    println!();
}
