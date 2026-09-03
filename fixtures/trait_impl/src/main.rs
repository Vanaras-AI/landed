trait Greet { fn greet(&self); }
struct A;
impl Greet for A {
    fn greet(&self) { hidden(); }
}
fn hidden() {}
fn main() { let g: &dyn Greet = &A; g.greet(); }
