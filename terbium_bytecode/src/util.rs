use std::{hash::{Hash, Hasher}, fmt::Debug};
use std::fmt::{Display, Formatter, Result as FmtResult};


#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct EqComparableFloat(pub f64);

impl EqComparableFloat {
    fn key(self) -> u64 {
        u64::from_be_bytes(self.0.to_be_bytes())
    }
}

impl From<f64> for EqComparableFloat {
    fn from(f: f64) -> Self {
        Self(f)
    }
}

impl From<EqComparableFloat> for f64 {
    fn from(f: EqComparableFloat) -> Self {
        f.0
    }
}

impl PartialEq for EqComparableFloat {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl PartialEq<f64> for EqComparableFloat {
    fn eq(&self, other: &f64) -> bool {
        self.key() == Self::from(other.to_owned()).key()
    }
}

impl Eq for EqComparableFloat {}

impl Hash for EqComparableFloat {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.key().hash(h);
    }
}

impl Default for EqComparableFloat {
    /// Returns the default value of 0.0
    fn default() -> Self {
        Self(0.0)
    }
}

impl Display for EqComparableFloat {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::EqComparableFloat;
    use std::collections::HashMap;

    #[test]
    pub fn test_float_eq() {
        assert_eq!(EqComparableFloat(0.1), EqComparableFloat(0.1));

        let mut sample = HashMap::<EqComparableFloat, u8>::with_capacity(1);
        sample.insert(EqComparableFloat(0.1), 0);

        assert_eq!(sample.get(&EqComparableFloat(0.1)), Some(&0_u8));
    }
}
