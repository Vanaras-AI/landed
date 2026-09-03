macro_rules! wrap { ($body:block) => { $body }; }
fn main() { run(); }
fn run() { wrap!({ called_from_macro(); }); }
fn called_from_macro() {}
#[cfg(test)]
mod tests {
    #[test] fn t() { super::called_from_macro(); }
}
