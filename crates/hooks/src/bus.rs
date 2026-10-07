use std::sync::Mutex;

use crate::plugins::PluginError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    ChooseTorrent,
    AddDownload,
    Transfer,
    Rename,
    Scrape,
    Login,
    CheckIn,
}

#[derive(Clone, Debug)]
pub struct HookEvent {
    pub step: Step,
}

pub struct Hook {
    step: Step,
    run: Box<dyn Fn(&HookEvent) -> Result<(), PluginError> + Send + Sync>,
}

impl Hook {
    pub fn new(
        step: Step,
        run: impl Fn(&HookEvent) -> Result<(), PluginError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            step,
            run: Box::new(run),
        }
    }
}

#[derive(Default)]
pub struct Bus {
    hooks: Mutex<Vec<Hook>>,
}

impl Bus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, hook: Hook) {
        tracing::debug!(step = ?hook.step, "注册钩子");
        self.hooks.lock().expect("hook bus").push(hook);
    }

    pub fn emit(&self, event: &HookEvent) -> Result<(), PluginError> {
        let hooks = self.hooks.lock().expect("hook bus");
        let mut fired = 0usize;
        for hook in hooks.iter() {
            if hook.step == event.step {
                fired += 1;
                if let Err(error) = (hook.run)(event) {
                    tracing::warn!(step = ?event.step, error = %error, "钩子执行失败");
                    return Err(error);
                }
            }
        }
        tracing::trace!(step = ?event.step, fired, "触发钩子");
        Ok(())
    }
}
