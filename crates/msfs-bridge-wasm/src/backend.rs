use core::fmt;

pub trait CommBusBackend: 'static {
    type Error: fmt::Display + fmt::Debug;
    type Subscription: 'static;

    fn subscribe(
        event: &str,
        callback: impl Fn(&str) + 'static,
    ) -> Result<Self::Subscription, Self::Error>;
    fn call(event: &str, data: &str) -> Result<(), Self::Error>;
}
