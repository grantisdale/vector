use regex::Regex;
use remap::prelude::*;
use std::cmp::min;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug)]
pub struct IpSubnet;

impl Function for IpSubnet {
    fn identifier(&self) -> &'static str {
        "ip_subnet"
    }

    fn parameters(&self) -> &'static [Parameter] {
        &[
            Parameter {
                keyword: "value",
                accepts: |v| matches!(v, Value::String(_)),
                required: true,
            },
            Parameter {
                keyword: "subnet",
                accepts: |v| matches!(v, Value::String(_)),
                required: true,
            },
        ]
    }

    fn compile(&self, mut arguments: ArgumentList) -> Result<Box<dyn Expression>> {
        let value = arguments.required_expr("value")?;
        let subnet = arguments.required_expr("subnet")?;

        Ok(Box::new(IpSubnetFn { value, subnet }))
    }
}

#[derive(Debug, Clone)]
struct IpSubnetFn {
    value: Box<dyn Expression>,
    subnet: Box<dyn Expression>,
}

impl IpSubnetFn {
    #[cfg(test)]
    fn new(value: Box<dyn Expression>, subnet: Box<dyn Expression>) -> Self {
        Self { value, subnet }
    }
}

impl Expression for IpSubnetFn {
    fn execute(&self, state: &mut state::Program, object: &mut dyn Object) -> Result<Value> {
        let value: IpAddr = {
            let bytes = required!(state, object, self.value, Value::String(v) => v);
            String::from_utf8_lossy(&bytes)
                .parse()
                .map_err(|err| format!("unable to parse IP address: {}", err))
        }?;

        let mask = {
            let bytes = required!(state, object, self.subnet, Value::String(v) => v);
            String::from_utf8_lossy(&bytes).into_owned()
        };

        let mask = if mask.starts_with("/") {
            // The parameter is a subnet.
            let subnet = parse_subnet(&mask)?;
            match value {
                IpAddr::V4(_) => {
                    if subnet > 32 {
                        return Err("subnet cannot be greater than 32 for ipv4 addresses".into());
                    }

                    ipv4_addr(get_mask_bits(subnet, 4))
                }
                IpAddr::V6(_) => {
                    if subnet > 128 {
                        return Err("subnet cannot be greater than 128 for ipv6 addresses".into());
                    }

                    ipv6_addr(get_mask_bits(subnet, 16))
                }
            }
        } else {
            // The parameter is a mask.
            mask.parse()
                .map_err(|err| format!("unable to parse mask: {}", err))?
        };

        Ok(Value::from(mask_ips(value, mask)?.to_string()))
    }

    fn type_def(&self, state: &state::Compiler) -> TypeDef {
        self.value
            .type_def(state)
            .fallible_unless(value::Kind::String)
            .merge(
                self.subnet
                    .type_def(state)
                    .fallible_unless(value::Kind::String),
            )
            .with_constraint(value::Kind::String)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map;

    remap::test_type_def![value_string {
        expr: |_| IpSubnetFn {
            value: Literal::from("192.168.0.1").boxed(),
            subnet: Literal::from("/1").boxed(),
        },
        def: TypeDef {
            kind: value::Kind::String,
            ..Default::default()
        },
    }];

    #[test]
    fn test_get_mask_bits() {
        assert_eq!(vec![255, 240, 0, 0], get_mask_bits(12, 4));
        assert_eq!(vec![255, 255, 0, 0], get_mask_bits(16, 4));
        assert_eq!(vec![255, 128, 0, 0], get_mask_bits(9, 4));
        assert_eq!(
            vec![255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0,],
            get_mask_bits(64, 16)
        );
    }

    #[test]
    fn ip_subnet() {
        let cases = vec![
            (
                map!["foo": "192.168.10.23"],
                Ok(Value::from("192.168.0.0")),
                IpSubnetFn::new(
                    Box::new(Path::from("foo")),
                    Box::new(Literal::from("255.255.0.0")),
                ),
            ),
            (
                map!["foo": "2404:6800:4003:c02::64"],
                Ok(Value::from("2400::")),
                IpSubnetFn::new(
                    Box::new(Path::from("foo")),
                    Box::new(Literal::from("ff00::")),
                ),
            ),
            (
                map!["foo": "192.168.10.23"],
                Ok(Value::from("192.168.0.0")),
                IpSubnetFn::new(Box::new(Path::from("foo")), Box::new(Literal::from("/16"))),
            ),
            (
                map!["foo": "192.168.10.23"],
                Ok(Value::from("192.160.0.0")),
                IpSubnetFn::new(Box::new(Path::from("foo")), Box::new(Literal::from("/12"))),
            ),
            (
                map!["foo": "2404:6800:4003:c02::64"],
                Ok(Value::from("2404:6800::")),
                IpSubnetFn::new(Box::new(Path::from("foo")), Box::new(Literal::from("/32"))),
            ),
        ];

        let mut state = state::Program::default();

        for (mut object, exp, func) in cases {
            let got = func
                .execute(&mut state, &mut object)
                .map_err(|e| format!("{:#}", anyhow::anyhow!(e)));

            assert_eq!(got, exp);
        }
    }
}

/// Parses a subnet in the form "/8" returns the number.
fn parse_subnet(subnet: &str) -> Result<u32> {
    let re = Regex::new(r"/(?P<subnet>\d*)").unwrap();
    let subnet = re
        .captures(subnet)
        .ok_or_else(|| format!("{} is not a valid subnet", subnet))?;

    let subnet = subnet["subnet"].parse().unwrap(); // The regex ensures these are only digits.

    Ok(subnet)
}

/// Masks the address by performing a bitwise AND between the two addresses.
fn mask_ips(ip: IpAddr, mask: IpAddr) -> Result<IpAddr> {
    match (ip, mask) {
        (IpAddr::V4(addr), IpAddr::V4(mask)) => {
            let addr: u32 = addr.into();
            let mask: u32 = mask.into();
            Ok(Ipv4Addr::from(addr & mask).into())
        }
        (IpAddr::V6(addr), IpAddr::V6(mask)) => {
            let mut masked = [0; 8];
            for i in 0..8 {
                masked[i] = addr.segments()[i] & mask.segments()[i];
            }

            Ok(IpAddr::from(masked))
        }
        (IpAddr::V6(_), IpAddr::V4(_)) => {
            Err("attempting to mask an ipv6 address with an ipv4 mask".into())
        }
        (IpAddr::V4(_), IpAddr::V6(_)) => {
            Err("attempting to mask an ipv4 address with an ipv6 mask".into())
        }
    }
}

/// Returns a vector with the left `subnet_bits` set to 1,
/// The remaining are set to 0, to make up a total length of `bytes`.
fn get_mask_bits(mut subnet_bits: u32, bytes: usize) -> Vec<u8> {
    let mut mask = Vec::with_capacity(bytes);

    while subnet_bits > 0 {
        let bits = min(subnet_bits, 8);
        let byte = 255 - (2u16.pow(8 - bits) - 1) as u8;
        mask.push(byte);

        subnet_bits -= bits
    }

    while mask.len() < bytes {
        mask.push(0);
    }

    mask
}

/// Take a vector of 4 bytes and returns an ipv4 IpAddr.
fn ipv4_addr(vec: Vec<u8>) -> IpAddr {
    debug_assert!(vec.len() == 4);
    Ipv4Addr::new(vec[0], vec[1], vec[2], vec[3]).into()
}

/// Take a vector of 16 bytes and returns an ipv6 IpAddr.
/// This can be made nicer in [1.48](https://blog.rust-lang.org/2020/11/19/Rust-1.48.html#library-changes)
fn ipv6_addr(vec: Vec<u8>) -> IpAddr {
    debug_assert!(vec.len() == 16);
    Ipv6Addr::from([
        vec[0], vec[1], vec[2], vec[3], vec[4], vec[5], vec[6], vec[7], vec[8], vec[9], vec[10],
        vec[11], vec[12], vec[13], vec[14], vec[15],
    ])
    .into()
}