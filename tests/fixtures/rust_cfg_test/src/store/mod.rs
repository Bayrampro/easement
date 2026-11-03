use testkit::Clock;

pub mod ok;

pub struct Db(pub Clock);

#[cfg(test)]
mod tests {
    use testkit::Clock;

    #[test]
    fn opens() {
        let _ = Clock;
    }
}
