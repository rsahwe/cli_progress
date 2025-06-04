use std::{
    array,
    borrow::Cow,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    thread::sleep,
    time::Duration,
};

use cli_progress::{CLIDisplayManager, CLIDisplayNodeType, erasing_println};
use rand::{Rng, rng};

fn main() {
    // Create CLIDisplayManager at the beginning
    let mut clidm = CLIDisplayManager::new(
        CLIDisplayNodeType::SpinningMessage(Cow::Borrowed("Progress bar example")),
        10,
    );

    // The progress bars are stores as Arc<AtomicU8>
    let bars: [Arc<AtomicU8>; 3] = array::from_fn(|_| Arc::new(AtomicU8::new(0)));

    // The progress bars are initialized in the CLIDisplayManager
    clidm.modify(|modify| {
        for bar in &bars {
            modify.push(CLIDisplayNodeType::ProgressBar(bar.clone()));
        }
    });

    let mut rng = rng();

    // This randomly advances the bars until they all finish
    while !bars.iter().all(|bar| bar.load(Ordering::Relaxed) == 100) {
        let mut rand = 0;

        if rng.random_bool(0.3) || bars[rand].load(Ordering::Relaxed) == 100 {
            rand += 1;
            if rng.random_bool(0.3) || bars[rand].load(Ordering::Relaxed) == 100 {
                rand += 1;
            }
        }

        if bars[rand].fetch_add(1, Ordering::Relaxed) == 99 {
            clidm.modify(|modify| {
                // Notice that this is possible
                erasing_println!(modify, "Bar {} completed!", rand);
            });
        }

        sleep(Duration::from_millis(9));
    }
}
