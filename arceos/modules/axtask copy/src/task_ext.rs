/// User-defined task extended data.
/// # Safety
/// See [`extern_trait`].
#[cfg(feature = "task-ext")]
#[extern_trait::extern_trait(
    /// The impl proxy type for [`TaskExt`].
    pub AxTaskExt
)]
pub unsafe trait TaskExt {
    /// Called when the task is switched in.
    fn on_enter(&self) {}
    /// Called when the task is switched out.
    fn on_leave(&self) {}
}