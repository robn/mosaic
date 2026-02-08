use std::str::FromStr;

#[derive(Debug, Clone)]
pub(crate) struct ConditionArg<T, C> {
    pub value: T,
    cond: Option<C>,
}

impl<T, C> ConditionArg<T, C>
where
    C: Comparator,
{
    pub fn matches(&self, v: C::Value) -> bool {
        self.cond.as_ref().map_or(true, |c| c.matches(v))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ConditionArgParseError;

impl<T, C> FromStr for ConditionArg<T, C>
where
    T: FromStr,
    C: FromStr,
{
    type Err = ConditionArgParseError;
    fn from_str(s: &str) -> Result<Self, ConditionArgParseError> {
        match s.split_once('@') {
            Some((n, c)) => Ok(Self {
                value: n.parse::<T>().map_err(|_| ConditionArgParseError)?,
                cond: Some(c.parse::<C>().map_err(|_| ConditionArgParseError)?),
            }),
            None => Ok(ConditionArg {
                value: s.parse::<T>().map_err(|_| ConditionArgParseError)?,
                cond: None,
            }),
        }
    }
}

pub(crate) trait Comparator {
    type Value;
    fn matches(&self, v: Self::Value) -> bool;
}

#[derive(Debug, Clone)]
pub(crate) enum OrderedComparator<T> {
    Equal(T),
    NotEqual(T),
    LessThan(T),
    LessThanOrEqual(T),
    GreaterThan(T),
    GreaterThanOrEqual(T),
}

impl<T> Comparator for OrderedComparator<T>
where
    T: PartialOrd,
{
    type Value = T;
    fn matches(&self, v: T) -> bool {
        match self {
            OrderedComparator::Equal(t) => v == *t,
            OrderedComparator::NotEqual(t) => v != *t,
            OrderedComparator::LessThan(t) => v < *t,
            OrderedComparator::LessThanOrEqual(t) => v <= *t,
            OrderedComparator::GreaterThan(t) => v > *t,
            OrderedComparator::GreaterThanOrEqual(t) => v >= *t,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OrderedComparatorParseError;

impl<T> FromStr for OrderedComparator<T>
where
    T: PartialOrd + PartialEq + FromStr,
{
    type Err = OrderedComparatorParseError;
    fn from_str(s: &str) -> Result<OrderedComparator<T>, OrderedComparatorParseError> {
        if let Some((o, n)) = s.split_at_checked(2) {
            match o {
                "==" => {
                    return Ok(OrderedComparator::Equal(
                        n.parse::<T>().map_err(|_| OrderedComparatorParseError)?,
                    ))
                }
                "!=" => {
                    return Ok(OrderedComparator::NotEqual(
                        n.parse::<T>().map_err(|_| OrderedComparatorParseError)?,
                    ))
                }
                "<=" => {
                    return Ok(OrderedComparator::LessThanOrEqual(
                        n.parse::<T>().map_err(|_| OrderedComparatorParseError)?,
                    ))
                }
                ">=" => {
                    return Ok(OrderedComparator::GreaterThanOrEqual(
                        n.parse::<T>().map_err(|_| OrderedComparatorParseError)?,
                    ))
                }
                _ => {}
            }
        }
        if let Some((o, n)) = s.split_at_checked(1) {
            match o {
                "=" => {
                    return Ok(OrderedComparator::Equal(
                        n.parse::<T>().map_err(|_| OrderedComparatorParseError)?,
                    ))
                }
                "!" => {
                    return Ok(OrderedComparator::NotEqual(
                        n.parse::<T>().map_err(|_| OrderedComparatorParseError)?,
                    ))
                }
                "<" => {
                    return Ok(OrderedComparator::LessThan(
                        n.parse::<T>().map_err(|_| OrderedComparatorParseError)?,
                    ))
                }
                ">" => {
                    return Ok(OrderedComparator::GreaterThan(
                        n.parse::<T>().map_err(|_| OrderedComparatorParseError)?,
                    ))
                }
                _ => {}
            }
        }
        Err(OrderedComparatorParseError)
    }
}
