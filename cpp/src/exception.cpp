// Native exception handler for Horizon (libnx).
//
// Background: a Mario 63 crash that fires only when the player is wearing
// the rocket-nozzle FLUDD bypasses Rust's `std::panic::set_hook` completely
// — no `=== PANIC ===` line ever reaches nxlink. That means the fault is
// not a Rust panic: it's a hardware exception (most likely a data abort
// from a NULL/invalid pointer in Mesa or a stack overflow inside an AS2
// hot path). On Horizon the kernel doesn't deliver SIGSEGV; it forwards
// the abort to libnx's `__libnx_exception_handler`, which by default just
// re-aborts. We override it here to dump the program counter, faulting
// address, and a full register snapshot to both nxlink stdout AND a
// persistent file on the SD card before the process dies.
//
// The handler runs on a dedicated 32 KB stack we reserve below — the
// faulting thread's own stack may itself be corrupted on entry.

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <switch.h>

extern "C" {

// Give the exception handler its own stack so we can still print/fopen even
// if the original thread blew its stack. `__nx_exception_stack_size` is a
// weak symbol exported by libnx; the linker picks up our value.
alignas(16) static u8 g_exception_stack[32 * 1024];
u8* __nx_exception_stack = g_exception_stack + sizeof(g_exception_stack);
u64 __nx_exception_stack_size = sizeof(g_exception_stack);

// `__libnx_exception_handler` is a weak symbol in libnx; defining it here
// overrides the default (which just re-aborts). The kernel populates the
// passed-in dump with the faulting thread's registers + fault info.
void __libnx_exception_handler(ThreadExceptionDump* ctx) {
    if (!ctx) return;

    // Re-entry guard: walking the faulting thread's stack below could itself
    // fault on a bad frame pointer, re-entering this handler. The process
    // aborts after the first crash anyway, so just bail on any nested entry
    // (never reset) rather than risk an infinite fault loop.
    static volatile bool s_handling = false;
    if (s_handling) return;
    s_handling = true;

    // Format the whole dump into a single stack buffer so the fputs/file
    // write happens as one shot (less chance of being torn by another
    // crash mid-write).
    // Anchor for symbolication. PC/LR carry the ASLR'd module base, so they
    // can't be fed to addr2line directly. We print the runtime address of a
    // known symbol in THIS module (the handler itself), so on the host:
    //   elf_addr = PC - REF + nm(__libnx_exception_handler)
    // then `aarch64-none-elf-addr2line -e cpp/flash-for-switch.elf -f -C <elf_addr>`
    // resolves the crashing source line. Same base cancels out, ASLR-proof.
    const unsigned long ref = (unsigned long)(uintptr_t)&__libnx_exception_handler;

    char buf[2048];
    int n = std::snprintf(buf, sizeof(buf),
        "\n=== NATIVE EXCEPTION ===\n"
        "error_desc = 0x%lx (0x100=InstrAbort 0x101=DataAbort 0x102=MisalignedPC\n"
        "                   0x103=MisalignedSP 0x104=Trap 0x106=SError 0x301=BadSVC)\n"
        "REF = 0x%016lx  (&__libnx_exception_handler; elf=PC-REF+nm(handler))\n"
        "PC  = 0x%016lx\n"
        "LR  = 0x%016lx\n"
        "SP  = 0x%016lx\n"
        "FP  = 0x%016lx\n"
        "FAR = 0x%016lx  (faulting address)\n"
        "ESR = 0x%08x   pstate = 0x%08x\n"
        "AFSR0=0x%08x   AFSR1=0x%08x\n"
        "x0 =0x%016lx  x1 =0x%016lx  x2 =0x%016lx  x3 =0x%016lx\n"
        "x4 =0x%016lx  x5 =0x%016lx  x6 =0x%016lx  x7 =0x%016lx\n"
        "x8 =0x%016lx  x9 =0x%016lx  x10=0x%016lx  x11=0x%016lx\n"
        "x12=0x%016lx  x13=0x%016lx  x14=0x%016lx  x15=0x%016lx\n"
        "x16=0x%016lx  x17=0x%016lx  x18=0x%016lx  x19=0x%016lx\n"
        "x20=0x%016lx  x21=0x%016lx  x22=0x%016lx  x23=0x%016lx\n"
        "x24=0x%016lx  x25=0x%016lx  x26=0x%016lx  x27=0x%016lx\n"
        "x28=0x%016lx\n"
        "========================\n",
        (unsigned long)ctx->error_desc,
        ref,
        (unsigned long)ctx->pc.x,
        (unsigned long)ctx->lr.x,
        (unsigned long)ctx->sp.x,
        (unsigned long)ctx->fp.x,
        (unsigned long)ctx->far.x,
        (unsigned)ctx->esr, (unsigned)ctx->pstate,
        (unsigned)ctx->afsr0, (unsigned)ctx->afsr1,
        (unsigned long)ctx->cpu_gprs[0].x,  (unsigned long)ctx->cpu_gprs[1].x,
        (unsigned long)ctx->cpu_gprs[2].x,  (unsigned long)ctx->cpu_gprs[3].x,
        (unsigned long)ctx->cpu_gprs[4].x,  (unsigned long)ctx->cpu_gprs[5].x,
        (unsigned long)ctx->cpu_gprs[6].x,  (unsigned long)ctx->cpu_gprs[7].x,
        (unsigned long)ctx->cpu_gprs[8].x,  (unsigned long)ctx->cpu_gprs[9].x,
        (unsigned long)ctx->cpu_gprs[10].x, (unsigned long)ctx->cpu_gprs[11].x,
        (unsigned long)ctx->cpu_gprs[12].x, (unsigned long)ctx->cpu_gprs[13].x,
        (unsigned long)ctx->cpu_gprs[14].x, (unsigned long)ctx->cpu_gprs[15].x,
        (unsigned long)ctx->cpu_gprs[16].x, (unsigned long)ctx->cpu_gprs[17].x,
        (unsigned long)ctx->cpu_gprs[18].x, (unsigned long)ctx->cpu_gprs[19].x,
        (unsigned long)ctx->cpu_gprs[20].x, (unsigned long)ctx->cpu_gprs[21].x,
        (unsigned long)ctx->cpu_gprs[22].x, (unsigned long)ctx->cpu_gprs[23].x,
        (unsigned long)ctx->cpu_gprs[24].x, (unsigned long)ctx->cpu_gprs[25].x,
        (unsigned long)ctx->cpu_gprs[26].x, (unsigned long)ctx->cpu_gprs[27].x,
        (unsigned long)ctx->cpu_gprs[28].x
    );

    if (n > 0) {
        // 1. nxlink stdout — may or may not flush before we abort.
        std::fputs(buf, stdout);
        std::fflush(stdout);
        // 2. Persistent log — survives a torn socket. Boot-replay code in
        // main.cpp will pick this up on the next launch and dump it again
        // to stdout, so we always see it via nxlink eventually.
        FILE* f = std::fopen("sdmc:/switch/ruffle-crash.log", "a");
        if (f) {
            std::fputs(buf, f);
            std::fflush(f);
            std::fclose(f);
        }

        // Backtrace via STACK SCAN. Rust is built without frame pointers, so
        // the x29 chain is unusable (FP isn't even 8-aligned at the fault). So
        // instead we scan the faulting thread's stack for 4-aligned words that
        // land inside this module's code (|word - REF| < a generous window) —
        // these are return-address candidates. The PC/LR only show the panic
        // machinery (panic_count/panic_with_hook); the real `panic!` caller is
        // among these. Printed REF-relative so the host can addr2line each. Noisy
        // (some false positives) but it climbs past the panic frames without
        // needing a frame-pointer rebuild.
        char bt[2048];
        int m = std::snprintf(bt, sizeof(bt),
            "=== STACK SCAN (ret-addr candidates, REF-relative; addr2line each) ===\n");
        uint64_t sp = ctx->sp.x;
        const int64_t WIN = 0x8000000; // ±128 MB around REF (module is big: Mesa+std)
        int found = 0;
        for (uint64_t a = sp; a < sp + 0x6000 && found < 48; a += 8) {
            uint64_t v = *(volatile uint64_t*)a;
            if ((v & 3) != 0) continue; // AArch64 instructions are 4-aligned
            int64_t d = (int64_t)(v - ref);
            if (d > -WIN && d < WIN) {
                m += std::snprintf(bt + m, sizeof(bt) - m,
                    "sp+0x%04lx: %s0x%lx\n",
                    (unsigned long)(a - sp),
                    d < 0 ? "-" : "+",
                    (unsigned long)(d < 0 ? -d : d));
                found++;
            }
            if (m > (int)sizeof(bt) - 48) break;
        }
        std::fputs(bt, stdout);
        std::fflush(stdout);
        FILE* fb = std::fopen("sdmc:/switch/ruffle-crash.log", "a");
        if (fb) {
            std::fputs(bt, fb);
            std::fflush(fb);
            std::fclose(fb);
        }

        // 3. Give nxlink ~500 ms to drain its TCP buffer before the kernel
        // tears the process down.
        svcSleepThread(500ULL * 1000 * 1000);
    }
}

} // extern "C"
