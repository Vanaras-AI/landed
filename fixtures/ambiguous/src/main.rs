fn main() { A.process(); }
struct A;
impl A { fn process(&self) {} }
struct B;
impl B { fn process(&self) {} }
#[cfg(test)]
mod tests {
    #[test] fn t() { super::B.process(); }
}
