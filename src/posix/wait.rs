use crate::posix::process::{PROCESS_TABLE, Pid, ProcState};
use core::task::{Poll, Waker};

/// Polls for the exit status of a child process.
/// If `pid` is -1, it waits for any child.
pub fn poll_waitpid(
    ppid: Pid,
    target_pid: Pid,
    waker: &Waker,
) -> Poll<Result<(Pid, i32), &'static str>> {
    let mut table = PROCESS_TABLE.lock();

    let parent = table.get(&ppid).cloned().ok_or("Parent not found")?;

    let mut found_zombie = None;

    {
        let children = parent.children.lock();

        for &child_pid in children.iter() {
            if target_pid == -1 || target_pid == child_pid {
                if let Some(child) = table.get(&child_pid) {
                    let state = child.state.lock();
                    if let ProcState::Zombie(status) = *state {
                        found_zombie = Some((child_pid, status));
                        break;
                    }
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

/// A fully synchronous waitpid for testing the mock system in kmain.
pub fn waitpid_sync(ppid: Pid, target_pid: Pid) -> Result<(Pid, i32), &'static str> {
    loop {
        // We simulate polling. In a real system, we'd sleep and be woken up.
        let mut table = PROCESS_TABLE.lock();
        let parent = table.get(&ppid).cloned().ok_or("Parent not found")?;

        let mut found_zombie = None;
        {
            let children = parent.children.lock();
            for &child_pid in children.iter() {
                if target_pid == -1 || target_pid == child_pid {
                    if let Some(child) = table.get(&child_pid) {
                        let state = child.state.lock();
                        if let ProcState::Zombie(status) = *state {
                            found_zombie = Some((child_pid, status));
                            break;
                        }
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

        // Normally we'd `wfi` here, but for the mock test we just return.
        // To avoid a tight loop freezing the test, we'll just return an error if no zombie is found immediately.
        return Err("No zombie found");
    }
}
