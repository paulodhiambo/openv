//! # Wait/Exit Handling
//!
//! This module provides wait/exit handling for processes. The
//! [`poll_waitpid`] function polls for the exit status of a child
//! process, reaping zombies when they become available.

use crate::posix::process::{PROCESS_TABLE, Pid, ProcState};
use core::task::{Poll, Waker};

/// Polls for the exit status of a child process.
///
/// If `target_pid` is `-1`, this function waits for any child. Otherwise,
/// it waits for the specific child with the given PID.
///
/// # Arguments
///
/// * `ppid` - The parent process ID.
/// * `target_pid` - The child PID to wait for, or `-1` for any child.
/// * `_waker` - A waker for async/await integration (unused in the
///   current implementation).
///
/// # Returns
///
/// - `Poll::Ready(Ok((pid, status)))` if a zombie child is found and reaped.
/// - `Poll::Pending` if no zombie is available yet.
/// - `Poll::Ready(Err("Parent not found"))` if the parent process doesn't exist.
pub fn poll_waitpid(
    ppid: Pid,
    target_pid: Pid,
    _waker: &Waker,
) -> Poll<Result<(Pid, i32), &'static str>> {
    let mut table = PROCESS_TABLE.lock();

    let parent = table.get(&ppid).cloned().ok_or("Parent not found")?;

    let mut found_zombie = None;

    {
        let children = parent.children.lock();

        for &child_pid in children.iter() {
            if (target_pid == -1 || target_pid == child_pid)
                && let Some(child) = table.get(&child_pid)
            {
                let state = child.state.lock();
                if let ProcState::Zombie(status) = *state {
                    found_zombie = Some((child_pid, status));
                    break;
                }
            }
        }
    }

    if let Some((zpid, status)) = found_zombie {
        // Reap the zombie
        table.remove(&zpid);
        let mut children = parent.children.lock();
        children.retain(|&p| p != zpid);

        Poll::Ready(Ok((zpid, status)))
    } else {
        // We would register the waker with the parent so it wakes when a child exits.
        // For our synchronous mock, we just return Pending.
        Poll::Pending
    }
}

/// A fully synchronous `waitpid` for testing the mock system in `kmain`.
///
/// # Arguments
///
/// * `ppid` - The parent process ID.
/// * `target_pid` - The child PID to wait for, or `-1` for any child.
///
/// # Returns
///
/// `Ok((pid, status))` if a zombie child is found and reaped, or
/// `Err("...")` if no zombie is available.
///
/// # Note
///
/// This function is intended for testing only. In a real system, the
/// process would sleep and be woken up by the scheduler when a child
/// exits.
pub fn waitpid_sync(ppid: Pid, target_pid: Pid) -> Result<(Pid, i32), &'static str> {
    // We simulate polling. In a real system, we'd sleep and be woken up.
    let mut table = PROCESS_TABLE.lock();
    let parent = table.get(&ppid).cloned().ok_or("Parent not found")?;

    let mut found_zombie = None;
    {
        let children = parent.children.lock();
        for &child_pid in children.iter() {
            if (target_pid == -1 || target_pid == child_pid)
                && let Some(child) = table.get(&child_pid)
            {
                let state = child.state.lock();
                if let ProcState::Zombie(status) = *state {
                    found_zombie = Some((child_pid, status));
                    break;
                }
            }
        }
    }

    if let Some((zpid, status)) = found_zombie {
        table.remove(&zpid);
        let mut children = parent.children.lock();
        children.retain(|&p| p != zpid);
        return Ok((zpid, status));
    }

    Err("No zombie found")
}
