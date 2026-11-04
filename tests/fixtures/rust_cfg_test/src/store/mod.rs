use testkit::Clock;

pub mod ok;
mod inner;

pub struct Db(pub Clock);

#[cfg(test)]
mod tests {
    use testkit::Clock;

    #[test]
    fn opens() {
        let _ = Clock;
    }
}

#[cfg(test)]
mod extra;
