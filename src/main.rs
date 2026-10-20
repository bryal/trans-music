mod cli;

fn main() {
    let opts = cli::parse();
    println!("{opts:#?}");
}
