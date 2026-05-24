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

    // Format the whole dump into a single stack buffer so the fputs/file
    // write happens as one shot (less chance of being torn by another
    // crash mid-write).
    char buf[2048];
    int n = std::snprintf(buf, sizeof(buf),
        "\n=== NATIVE EXCEPTION ===\n"
        "error_desc = 0x%lx (0x100=InstrAbort 0x101=DataAbort 0x102=MisalignedPC\n"
        "                   0x103=MisalignedSP 0x104=Trap 0x106=SError 0x301=BadSVC)\n"
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
        // 3. Give nxlink ~500 ms to drain its TCP buffer before the kernel
        // tears the process down.
        svcSleepThread(500ULL * 1000 * 1000);
    }
}

} // extern "C"
