// Prints the detected game installs as JSON. Used for the live check on a member PC
// (prototype M2) and by developers; the desktop app calls fm_platform::detect_games directly.
// "--timing" prints the cost of a first poll (install scan and processes) and of a second
// poll (kept process list, no install scan), which is what the app watcher pays every 2 s.
fn main() {
    if std::env::args().any(|a| a == "--timing") {
        let t = std::time::Instant::now();
        let mut d = fm_platform::Detector::new();
        d.poll(std::time::Duration::ZERO);
        let first = t.elapsed();
        let t = std::time::Instant::now();
        d.poll(std::time::Duration::from_secs(3600));
        println!("first poll {:?}, next poll {:?}", first, t.elapsed());
        return;
    }
    let installs = fm_platform::detect_games();
    println!("{}", serde_json::to_string_pretty(&installs).expect("serialize"));
}
