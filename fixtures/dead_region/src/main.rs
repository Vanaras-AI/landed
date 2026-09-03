fn main() { live(); }
fn live() {}
pub fn dead_entry() { middle(); }
fn middle() { leaf(); }
fn leaf() {}
#[cfg(test)]
mod tests {
    #[test] fn t() { super::dead_entry(); }
}
