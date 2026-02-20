extern "C" { fn stext(); }
fn main() {
    let x = unsafe { stext as usize };
    println!("{}", x);
}
