fn main() {
    vdso_helper::mut_cfg! {
        const QUEUE_LEN: usize = 4096;
        const ARRAY_LEN: usize = 64;
        // const BOOL_TEST: bool = true;
        // const EXPR_TEST: usize = QUEUE_LEN / 2;
        // const FLOAT_TEST: f64 = 3.14159;
    }
}
