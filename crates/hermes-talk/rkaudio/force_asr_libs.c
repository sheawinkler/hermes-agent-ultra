/* Small wrapper that calls librockasr and librockx_modules initialization.
   Compiled with cc crate into a static lib, forcing the linker to keep
   these shared libs in NEEDED so their ELF constructors can register the
   LLMASR module. */
extern void GetRockXModuleASR(void);
extern void RockXFeatureInit(void);

void force_asr_libs_init(void) {
    GetRockXModuleASR();
    RockXFeatureInit();
}
