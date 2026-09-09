//! Agent 进程管理工具
//!
//! 提供进程启动、停止、信号发送等工具函数。
//! 主要逻辑在 [`crate::agent::AgentManager`] 中，这里提供辅助函数。

use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

/// 构建 Agent 启动命令
pub fn build_command(
    entrypoint: &str,
    work_dir: &Path,
    env: &std::collections::HashMap<String, String>,
) -> Command {
    let mut cmd = Command::new(entrypoint);
    cmd.current_dir(work_dir)
        .envs(env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

/// 检查进程是否仍在运行
pub async fn is_running(child: &mut tokio::process::Child) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => false,
        Ok(None) => true,
        Err(_) => false,
    }
}
