fn main() { live(); }
fn live() {}
pub fn only_tests_call_me() {}
#[cfg(test)]
mod tests {
    #[test] fn t() { super::only_tests_call_me(); }
}
