fn main() { driver(); }
/// `limit` is a test hook: the fastest path skips it.
/// The latest revision keeps this documented but live.
fn driver() { real_work(); }
fn real_work() {}
#[cfg(test)]
mod tests {
    #[test] fn t() { super::driver(); }
}
