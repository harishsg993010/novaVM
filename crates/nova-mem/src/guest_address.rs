use std::fmt;
use std::ops::{Add, Sub};

/// A guest physical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct GuestAddress(pub u64);

impl GuestAddress {
    /// Create a new guest address.
    pub fn new(addr: u64) -> Self {
        Self(addr)
    }

    /// Returns the raw u64 value.
    pub fn raw(self) -> u64 {
        self.0
    }

    /// Checked addition — returns None on overflow.
    pub fn checked_add(self, offset: u64) -> Option<Self> {
        self.0.checked_add(offset).map(Self)
    }

    /// Checked subtraction — returns None on underflow.
    pub fn checked_sub(self, other: Self) -> Option<u64> {
        self.0.checked_sub(other.0)
    }

    /// Returns the offset from `base`, or None if self < base.
    pub fn offset_from(self, base: Self) -> Option<u64> {
        self.checked_sub(base)
    }
}

impl Add<u64> for GuestAddress {
    type Output = Self;

    fn add(self, rhs: u64) -> Self {
        Self(self.0 + rhs)
    }
}

impl Sub<GuestAddress> for GuestAddress {
    type Output = u64;

    fn sub(self, rhs: GuestAddress) -> u64 {
        self.0 - rhs.0
    }
}

impl fmt::Display for GuestAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl From<u64> for GuestAddress {
    fn from(addr: u64) -> Self {
        Self(addr)
    }
}

impl From<GuestAddress> for u64 {
    fn from(addr: GuestAddress) -> u64 {
        addr.0
    }
}
