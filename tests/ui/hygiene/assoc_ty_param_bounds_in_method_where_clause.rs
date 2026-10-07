//@ build
//@ stderr: empty

pub trait Layer<S> {
    type Service;

    fn layer(&self, inner: S) -> Self::Service;
}

pub struct Wrap<A, B>(pub A, pub B);

pub trait Named<L> {
    // TEST: The bound on the trait's parameter is only in the method's where clause.
    fn named_layer<S: Clone>(&self, service: S) -> Wrap<L::Service, S> where L: Layer<S>;
}

pub struct Stack<L>(pub L);

impl<L> Named<L> for Stack<L> {
    fn named_layer<S: Clone>(&self, service: S) -> Wrap<L::Service, S> where L: Layer<S> {
        let wrapped: L::Service = self.0.layer(service.clone());
        Wrap(wrapped, service)
    }
}

pub trait Service<Req> {
    type Error;

    fn ready(&mut self) -> Result<(), Self::Error>;
}

pub struct Client<T>(pub T);

impl<T> Client<T> {
    pub async fn ready(&mut self) -> Result<(), T::Error> where T: Service<u8> {
        let result: Result<(), T::Error> = self.0.ready();
        result
    }
}

struct Double;

impl Layer<u8> for Double {
    type Service = u16;

    fn layer(&self, inner: u8) -> u16 { inner as u16 * 2 }
}

struct Ready;

impl Service<u8> for Ready {
    type Error = ();

    fn ready(&mut self) -> Result<(), ()> { Ok(()) }
}

#[test]
fn test() {
    let Wrap(service, inner) = Stack(Double).named_layer(3_u8);
    assert_eq!((service, inner), (6, 3));

    let _ = Client(Ready).ready();
}
