fn main() { start(); }
fn start() { helper(); }
fn helper() {}
#[cfg(test)]
mod tests {
    #[test] fn t() { super::start(); }
}
