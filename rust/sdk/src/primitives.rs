use crate::{AxiomDecode, AxiomEncode, Result, SdkError};
use alloc::{format, string::String};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cents(u64);

impl Cents {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn from_dollars(dollars: u64) -> Result<Self> {
        dollars
            .checked_mul(100)
            .map(Self)
            .ok_or_else(|| SdkError::invalid_input("dollar amount exceeds Cents range"))
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn min(self, other: Self) -> Self {
        if self.0 <= other.0 {
            self
        } else {
            other
        }
    }

    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or_else(|| SdkError::invalid_input("currency subtraction would be negative"))
    }

    pub fn as_currency(self) -> String {
        format!("${}.{:02}", self.0 / 100, self.0 % 100)
    }

    pub fn as_saving(self) -> String {
        format!("−{}", self.as_currency())
    }
}

impl AxiomEncode for Cents {
    fn encode(self) -> crate::abi::Value {
        crate::abi::Value::Unsigned(self.0)
    }
}

impl AxiomDecode for Cents {
    fn decode(value: &crate::abi::Value) -> Result<Self> {
        u64::decode(value).map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Percentage {
    basis_points: u16,
}

impl Percentage {
    pub fn from_whole(value: u16) -> Result<Self> {
        if value > 100 {
            return Err(SdkError::invalid_input(
                "percentage must be between 0 and 100",
            ));
        }
        Ok(Self {
            basis_points: value * 100,
        })
    }

    pub fn from_basis_points(value: u16) -> Result<Self> {
        if value > 10_000 {
            return Err(SdkError::invalid_input(
                "basis points must be between 0 and 10000",
            ));
        }
        Ok(Self {
            basis_points: value,
        })
    }

    pub const fn basis_points(self) -> u16 {
        self.basis_points
    }

    pub fn of(self, amount: Cents) -> Result<Cents> {
        amount
            .value()
            .checked_mul(u64::from(self.basis_points))
            .map(|value| Cents::new(value / 10_000))
            .ok_or_else(|| SdkError::invalid_input("percentage calculation overflowed"))
    }
}

impl AxiomEncode for Percentage {
    fn encode(self) -> crate::abi::Value {
        crate::abi::Value::Unsigned(u64::from(self.basis_points))
    }
}

impl AxiomDecode for Percentage {
    fn decode(value: &crate::abi::Value) -> Result<Self> {
        Self::from_basis_points(u16::decode(value)?)
    }
}

macro_rules! unsigned_primitive {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(u64);

        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            pub const fn value(self) -> u64 {
                self.0
            }
        }

        impl AxiomEncode for $name {
            fn encode(self) -> crate::abi::Value {
                crate::abi::Value::Unsigned(self.0)
            }
        }

        impl AxiomDecode for $name {
            fn decode(value: &crate::abi::Value) -> Result<Self> {
                u64::decode(value).map(Self)
            }
        }
    };
}

unsigned_primitive!(TimestampMs);
unsigned_primitive!(DurationMs);
unsigned_primitive!(Revision);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Id(String);

impl Id {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(SdkError::invalid_input(
                "ID must contain 1-256 non-control characters",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AxiomEncode for Id {
    fn encode(self) -> crate::abi::Value {
        crate::abi::Value::String(self.0)
    }
}

impl AxiomDecode for Id {
    fn decode(value: &crate::abi::Value) -> Result<Self> {
        Self::new(String::decode(value)?)
    }
}
