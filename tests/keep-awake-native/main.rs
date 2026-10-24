#[path = "../../src/app/keep_awake.rs"]
mod keep_awake;

fn main() {
    // A compile check must never acquire a power request as a side effect.
    keep_awake::KeepAwake::default().set(false);
}
