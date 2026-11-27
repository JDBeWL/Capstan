//! 非 Windows 平台下任务栏集成整体不可用。

use std::sync::{Arc, Mutex};

use crate::{Art, Labels, TaskbarAction};

pub struct Taskbar;

impl Taskbar {
    pub fn attach(_hwnd: isize, _labels: Labels) -> Option<Self> {
        None
    }

    pub fn actions(&self) -> Arc<Mutex<Vec<TaskbarAction>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    pub fn set_art(&self, _art: Option<Art>) {}

    pub fn set_track(&self, _title: &str, _enabled: bool) {}

    pub fn set_playing(&self, _playing: bool) {}

    pub fn set_liked(&self, _liked: bool) {}
}
