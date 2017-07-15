//! A simple reverse proxy, to be used with Hyper and Tokio.

extern crate futures;
#[macro_use]
extern crate hyper;
#[macro_use]
extern crate lazy_static;
extern crate unicase;
extern crate void;

use futures::future::Future;
use hyper::{Body, Request, Response, StatusCode};
use hyper::server::Service;
use hyper::header::Headers;
use std::marker::PhantomData;
use std::net::IpAddr;
use void::Void;

fn is_hop_header(name: &str) -> bool {
    use unicase::Ascii;

    // A list of the headers, using `unicase` to help us compare without
    // worrying about the case, and `lazy_static!` to prevent reallocation
    // of the vector.
    lazy_static! {
        static ref HOP_HEADERS: Vec<Ascii<&'static str>> = vec![
            Ascii::new("Connection"),
            Ascii::new("Keep-Alive"),
            Ascii::new("Proxy-Authenticate"),
            Ascii::new("Proxy-Authorization"),
            Ascii::new("Te"),
            Ascii::new("Trailers"),
            Ascii::new("Transfer-Encoding"),
            Ascii::new("Upgrade"),
        ];
    }

    HOP_HEADERS.iter().any(|h| h == &name)
}

/// Returns a clone of the headers without the [hop-by-hop headers].
///
/// [hop-by-hop headers]: http://www.w3.org/Protocols/rfc2616/rfc2616-sec13.html
fn remove_hop_headers(headers: &Headers) -> Headers {
    headers
        .iter()
        .filter(|header| is_hop_header(header.name()))
        .collect()
}

// TODO: use https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Forwarded ?
header! {
    /// `X-Forwarded-For` header.
    ///
    /// The `X-Forwarded-For` header describes the path of
    /// proxies this request has been forwarded through.
    ///
    /// # Example Values
    ///
    /// * `2001:db8:85a3:8d3:1319:8a2e:370:7348`
    /// * `203.0.113.195`
    /// * `203.0.113.195, 70.41.3.18, 150.172.238.178`
    ///
    /// # References
    ///
    /// - [MDN](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/X-Forwarded-For)
    /// - [Wikipedia](https://en.wikipedia.org/wiki/X-Forwarded-For)
    (XForwardedFor, "X-Forwarded-For") => (IpAddr)+

    // test_x_forwarded_for {
    //     // Testcases from MDN
    //     test_header!(test1, vec![b"2001:db8:85a3:8d3:1319:8a2e:370:7348"]);
    //     test_header!(test2, vec![b"203.0.113.195"]);
    //     test_header!(test3, vec![b"203.0.113.195, 70.41.3.18, 150.172.238.178"]);
    // }
}

fn create_proxied_response<B>(mut response: Response<B>) -> Response<B> {
    *response.headers_mut() = remove_hop_headers(response.headers());
    response
}

/// A `Service` that takes an incoming request, sends it to a given `Client`, then proxies back
/// the response.
///
/// The implementation ensures that [Hop-by-hop headers] are stripped correctly in both directions,
/// and adds the client's IP address to a comma-space-separated list of forwarding addresses in the
/// `X-Forwarded-For` header.
///
/// The implementation is based on Go's [`httputil.ReverseProxy`].
///
/// [Hop-by-hop headers]: http://www.w3.org/Protocols/rfc2616/rfc2616-sec13.html
/// [`httputil.ReverseProxy`]: https://golang.org/pkg/net/http/httputil/#ReverseProxy
pub struct ReverseProxy<C: Service, B = Body> {
    client: C,
    remote_ip_addr: Option<IpAddr>,
    _pantom_data: PhantomData<B>,
}

impl<C: Service, B> ReverseProxy<C, B> {
    /// Construct a reverse proxy that dispatches to the given client.
    pub fn new(client: C, remote_ip_addr: Option<IpAddr>) -> ReverseProxy<C, B> {
        ReverseProxy {
            client,
            remote_ip_addr,
            _pantom_data: PhantomData,
        }
    }

    fn create_proxied_request(&self, mut request: Request<B>) -> Request<B> {
        *request.headers_mut() = remove_hop_headers(request.headers());

        // Add forwarding information in the headers
        if let Some(ip_addr) = self.remote_ip_addr {
            // This is kind of ugly because of borrowing. Maybe hyper's `Headers` object
            // could use an entry API like `std::collections::HashMap`?
            if request.headers().has::<XForwardedFor>() {
                if let Some(prior) = request.headers_mut().get_mut::<XForwardedFor>() {
                    prior.0.push(ip_addr);
                }
            } else {
                let header = XForwardedFor(vec![ip_addr]);
                request.headers_mut().set(header);
            }
        }

        request
    }
}

impl<C, B> Service for ReverseProxy<C, B>
where
    B: 'static,
    C: Service<Request = Request<B>, Response = Response<B>>,
    C::Error: 'static + std::fmt::Display,
    C::Future: 'static,
{
    type Request = Request<B>;
    type Response = Response<B>;
    type Error = Void;
    type Future = Box<Future<Item = Response<B>, Error = Void>>;

    fn call(&self, request: Self::Request) -> Self::Future {
        let proxied_request = self.create_proxied_request(request);

        Box::new(self.client.call(proxied_request).then(|response| {
            Ok(match response {
                Ok(response) => create_proxied_response(response),
                Err(error) => {
                    println!("Error: {}", error); // TODO: Configurable logging
                    Response::new().with_status(StatusCode::InternalServerError)
                    // TODO: handle trailers
                }
            })
        }))
    }
}