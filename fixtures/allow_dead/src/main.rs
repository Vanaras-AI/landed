fn main() { live(); }
fn live() {}
#[allow(dead_code)]
pub fn deliberately_unused() {}
#[cfg(test)]
mod tests {
    #[test] fn t() { super::deliberately_unused(); }
}
