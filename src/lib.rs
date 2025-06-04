//! Dynamic progress and process display library
//!
//! To use this, construct a [CLIDisplayManager], whose output can be changed with the [CLIDisplayManager::modify] function,
//! and drop it on completion.
//!
//! While the [CLIDisplayManager] is in use, no other [CLIDisplayManager] should be active,
//! however stdout can still be used through the [erasing_println] macro during [modify](CLIDisplayManager::modify) calls and it will appear in front of the displayed progress/process.
//!
//! Currently there are three types of displays:
//!
//! - [Just text](CLIDisplayNodeType::Message)
//! - [Text with a progress spinner at the end](CLIDisplayNodeType::SpinningMessage)
//! - [A progress bar whose progress can be set through an `Arc<AtomicU8>`](CLIDisplayNodeType::ProgressBar)
//!
//! Example with progress bars:
//! `cargo run --example progress`
//! ```
#![doc = include_str!("../examples/progress.rs")]
//! ```

#![deny(missing_docs)]

use std::{
    borrow::Cow,
    io::{Write, stdout},
    mem::forget,
    ops::Neg,
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering::*},
    },
    thread::{Builder, JoinHandle},
    time::Duration,
};

const CURSOR_HIDE: &str = "\x1B[?25l";
const CURSOR_SHOW: &str = "\x1B[?25h";
const ERASE_LINE: &str = "\x1b[2K\r";
const CURSOR_UP: &str = "\x1b[1A";

#[doc(hidden)]
pub const _ERASE_LINE: &str = ERASE_LINE;

struct CursorHideGuard;

impl CursorHideGuard {
    fn new() -> Self {
        print!("{}", CURSOR_HIDE);
        let _ = stdout().flush();
        CursorHideGuard
    }
}

impl Drop for CursorHideGuard {
    fn drop(&mut self) {
        print!("{}", CURSOR_SHOW);
        let _ = stdout().flush();
    }
}

/// This is the core struct of the library.
/// Everything is managed here.
/// Create this with the initial root item and a refresh rate and drop it when done.
pub struct CLIDisplayManager {
    root: Arc<RwLock<CLIDisplayNode>>,
    cv: Arc<Condvar>,
    mutex: Arc<Mutex<()>>,
    self_handle: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    tick_counter: Arc<AtomicUsize>,
    _cursor_visibility_guard: CursorHideGuard,
}

impl CLIDisplayManager {
    /// Creates a [CLIDisplayManager] with a specified root node and a tick rate at which it updates
    pub fn new(root_node: CLIDisplayNodeType, tick_rate: u32) -> Self {
        let _ = enable_ansi_support::enable_ansi_support();

        let mut clidm = Self {
            root: RwLock::new(CLIDisplayNode::new(root_node)).into(),
            cv: Condvar::new().into(),
            mutex: Mutex::new(()).into(),
            self_handle: None,
            stop: AtomicBool::new(false).into(),
            tick_counter: AtomicUsize::new(0).into(),
            _cursor_visibility_guard: CursorHideGuard::new(),
        };

        let stop = clidm.stop.clone();
        let cv = clidm.cv.clone();
        let mutex = clidm.mutex.clone();
        let root = clidm.root.clone();
        let tick_counter = clidm.tick_counter.clone();

        clidm.self_handle.replace(
            Builder::new()
                .name("CLIDisplayManagerThread".to_string())
                .spawn(move || {
                    let mut guard = mutex
                        .lock()
                        .expect("Poisoned mutex in CLIDisplayManagerThread!!!");

                    let node = root
                        .read()
                        .expect("Poisoned rwlock in CLIDisplayManagerThread!!!");
                    node.display(0, tick_counter.load(Relaxed), true);
                    node.go_back();
                    drop(node);

                    while !stop.load(Relaxed) {
                        let node = root
                            .read()
                            .expect("Poisoned rwlock in CLIDisplayManagerThread!!!");
                        node.display(0, tick_counter.load(Relaxed), true);
                        node.go_back();
                        drop(node);
                        print!("\r");

                        tick_counter.fetch_add(1, Relaxed);
                        if tick_rate != 0 {
                            guard = cv
                                .wait_timeout(guard, Duration::from_secs(1) / tick_rate)
                                .expect("Poisoned condition variable in CLIDisplayManagerThread!!!")
                                .0;
                        } else {
                            guard = cv.wait(guard).expect(
                                "Poisoned condition variable in CLIDisplayManagerThread!!!",
                            );
                        }
                    }
                })
                .unwrap(),
        );

        clidm
    }

    /// Modifies a [CLIDisplayManager]s output through a [CLIModificationElement] handle that gets passed to a callback
    pub fn modify<F: FnOnce(&mut CLIModificationElement) -> ()>(&mut self, f: F) {
        let guard = self.mutex.lock();

        let mut modification_element = CLIModificationElement {
            root_node: &self.root,
            additions: 0,
        };

        f(&mut modification_element);

        let removed_lines = modification_element.additions.neg().max(0);

        drop(modification_element);

        let node = self
            .root
            .read()
            .expect("Poisoned rwlock in CLIDisplayManagerThread!!!");
        node.display(0, self.tick_counter.load(Relaxed), true);

        for i in 1..=removed_lines {
            print!("{}", ERASE_LINE);

            if i != removed_lines {
                println!("");
            }
        }

        for _ in 1..removed_lines {
            print!("{}", CURSOR_UP);
        }

        node.go_back();
        drop(node);
        print!("\r");
        let _ = stdout().flush();

        drop(guard);
    }
}

impl Drop for CLIDisplayManager {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);

        self.cv.notify_all();

        self.self_handle.take().unwrap().join().unwrap();
    }
}

/// This is the struct through which the output of a [CLIDisplayManager] can be changed.
pub struct CLIModificationElement<'a> {
    root_node: &'a RwLock<CLIDisplayNode>,
    additions: isize,
}

impl<'a> CLIModificationElement<'a> {
    /// Removes the last displayed item
    pub fn pop(&mut self) {
        self.additions -= 1;

        let mut node = self
            .root_node
            .write()
            .expect("Poisoned rwlock in CLIModificationElement!!!");

        if node.sub_nodes.len() == 0 {
            self.additions += 1;
            return;
        }

        let mut mapped_node = &mut *node;

        while mapped_node.sub_nodes.last().unwrap().sub_nodes.len() != 0 {
            mapped_node = mapped_node.sub_nodes.last_mut().unwrap();
        }

        forget(mapped_node.sub_nodes.pop());
    }

    /// Adds another parallel task or subtask if only the root node is present
    pub fn push(&mut self, node_type: CLIDisplayNodeType) {
        self.additions += 1;

        let mut node = self
            .root_node
            .write()
            .expect("Poisoned rwlock in CLIModificationElement!!!");

        if node.sub_nodes.len() == 0 {
            drop(node);

            self.additions -= 1;
            return Self::make_sub(self, node_type);
        }

        let mut mapped_node = &mut *node;

        while mapped_node.sub_nodes.last().unwrap().sub_nodes.len() != 0 {
            mapped_node = mapped_node.sub_nodes.last_mut().unwrap();
        }

        mapped_node.sub_nodes.push(CLIDisplayNode::new(node_type));
    }

    /// Makes a new subtask for the current task
    pub fn make_sub(&mut self, node_type: CLIDisplayNodeType) {
        self.additions += 1;

        let mut node = self
            .root_node
            .write()
            .expect("Poisoned rwlock in CLIModificationElement!!!");

        let mut last_node = &mut *node;

        while last_node.sub_nodes.len() != 0 {
            last_node = last_node.sub_nodes.last_mut().unwrap();
        }

        last_node.sub_nodes.push(CLIDisplayNode::new(node_type));
    }

    /// Replaces the root node with a different one
    pub fn replace_root(&mut self, node_type: CLIDisplayNodeType) {
        self.root_node
            .write()
            .expect("Poisoned rwlock in CLIModificationElement!!!")
            .node_type = node_type;
    }
}

struct CLIDisplayNode {
    node_type: CLIDisplayNodeType,
    sub_nodes: Vec<CLIDisplayNode>,
}

impl CLIDisplayNode {
    fn new(node_type: CLIDisplayNodeType) -> Self {
        Self {
            node_type,
            sub_nodes: Vec::new(),
        }
    }

    fn display(&self, depth: usize, tick_counter: usize, last: bool) {
        print!("{}", ERASE_LINE);
        if depth != 0 {
            for _ in 1..depth {
                print!("  ");
            }

            if last {
                print!("\u{2514}\u{2574}");
            } else {
                print!("\u{251C}\u{2574}");
            }
        }

        self.node_type.display(tick_counter);

        for (index, sub_node) in self.sub_nodes.iter().enumerate() {
            sub_node.display(depth + 1, tick_counter, index + 1 == self.sub_nodes.len());
        }
    }

    fn go_back(&self) {
        for sub_node in self.sub_nodes.iter() {
            sub_node.go_back();
        }

        print!("{}", CURSOR_UP);
    }
}

impl Drop for CLIDisplayNode {
    fn drop(&mut self) {
        println!("");
    }
}

/// All possible display node types.
pub enum CLIDisplayNodeType {
    /// Just text
    Message(Cow<'static, str>),
    /// Text with an animated spinner at the end
    SpinningMessage(Cow<'static, str>),
    /// A controllable progress bar
    ProgressBar(Arc<AtomicU8>),
}

impl CLIDisplayNodeType {
    fn display(&self, tick_counter: usize) {
        match self {
            CLIDisplayNodeType::Message(cow) => println!("{}", cow),
            CLIDisplayNodeType::SpinningMessage(cow) => {
                println!("{} {}", cow, "/-\\|".chars().nth(tick_counter % 4).unwrap())
            }
            CLIDisplayNodeType::ProgressBar(progress) => {
                let mut lock = stdout().lock();
                let progress = (progress.load(Relaxed) / 5).clamp(0, 20);

                let _ = write!(lock, "[");

                for _ in 0..progress {
                    let _ = write!(lock, "#");
                }

                if progress != 20 {
                    let _ = write!(lock, "{}", "/-\\|".chars().nth(tick_counter % 4).unwrap());
                }

                for _ in progress..19 {
                    let _ = write!(lock, " ");
                }

                let _ = writeln!(lock, "]");
            }
        }
    }
}

/// This macro can be used in modify calls to add lines to stdout without interrupting the [CLIDisplayManager]
#[macro_export]
macro_rules! erasing_println {
    ($me:ident) => {{
        let _: &mut $crate::CLIModificationElement = $me;
        print!("{}\n", $crate::_ERASE_LINE)
    }};
    ($me:ident, $($arg:tt)*) => {{
        let _: &mut $crate::CLIModificationElement = $me;
        print!("{}{}\n", $crate::_ERASE_LINE, format_args!($($arg)*));
    }};
}
