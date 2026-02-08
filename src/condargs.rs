use std::str::FromStr;

pub(crate) fn percent_threshold(s: &str) -> Result<PercentThreshold, String> {
    s.parse::<PercentThreshold>()
        .map_err(|e| format!("{:?}", e))
}

#[derive(Debug, Clone)]
pub(crate) struct PercentThreshold {
    pub value: i32,
    compare: Option<CompareThreshold<f32>>,
}

impl PercentThreshold {
    pub fn matches(&self, v: f32) -> bool {
        self.compare.as_ref().map_or(true, |c| c.matches(v))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PercentThresholdParseError;

impl FromStr for PercentThreshold {
    type Err = PercentThresholdParseError;
    fn from_str(s: &str) -> Result<PercentThreshold, PercentThresholdParseError> {
        match s.split_once('@') {
            Some((n, c)) => Ok(PercentThreshold {
                value: n.parse::<i32>().map_err(|_| PercentThresholdParseError)?,
                compare: Some(
                    c.parse::<CompareThreshold<f32>>()
                        .map_err(|_| PercentThresholdParseError)?,
                ),
            }),
            None => Ok(PercentThreshold {
                value: s.parse::<i32>().map_err(|_| PercentThresholdParseError)?,
                compare: None,
            }),
        }
    }
}

#[derive(Debug, Clone)]
enum CompareThreshold<T> {
    Equal(T),
    NotEqual(T),
    LessThan(T),
    LessThanOrEqual(T),
    GreaterThan(T),
    GreaterThanOrEqual(T),
}

impl<T> CompareThreshold<T>
where
    T: PartialOrd,
{
    fn matches(&self, v: T) -> bool {
        match self {
            CompareThreshold::Equal(t) => v == *t,
            CompareThreshold::NotEqual(t) => v != *t,
            CompareThreshold::LessThan(t) => v < *t,
            CompareThreshold::LessThanOrEqual(t) => v <= *t,
            CompareThreshold::GreaterThan(t) => v > *t,
            CompareThreshold::GreaterThanOrEqual(t) => v >= *t,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CompareThresholdParseError;

impl<T> FromStr for CompareThreshold<T>
where
    T: PartialOrd + PartialEq + FromStr,
{
    type Err = CompareThresholdParseError;
    fn from_str(s: &str) -> Result<CompareThreshold<T>, CompareThresholdParseError> {
        if let Some((o, n)) = s.split_at_checked(2) {
            match o {
                "==" => {
                    return Ok(CompareThreshold::Equal(
                        n.parse::<T>().map_err(|_| CompareThresholdParseError)?,
                    ))
                }
                "!=" => {
                    return Ok(CompareThreshold::NotEqual(
                        n.parse::<T>().map_err(|_| CompareThresholdParseError)?,
                    ))
                }
                "<=" => {
                    return Ok(CompareThreshold::LessThanOrEqual(
                        n.parse::<T>().map_err(|_| CompareThresholdParseError)?,
                    ))
                }
                ">=" => {
                    return Ok(CompareThreshold::GreaterThanOrEqual(
                        n.parse::<T>().map_err(|_| CompareThresholdParseError)?,
                    ))
                }
                _ => {}
            }
        }
        if let Some((o, n)) = s.split_at_checked(1) {
            match o {
                "=" => {
                    return Ok(CompareThreshold::Equal(
                        n.parse::<T>().map_err(|_| CompareThresholdParseError)?,
                    ))
                }
                "!" => {
                    return Ok(CompareThreshold::NotEqual(
                        n.parse::<T>().map_err(|_| CompareThresholdParseError)?,
                    ))
                }
                "<" => {
                    return Ok(CompareThreshold::LessThan(
                        n.parse::<T>().map_err(|_| CompareThresholdParseError)?,
                    ))
                }
                ">" => {
                    return Ok(CompareThreshold::GreaterThan(
                        n.parse::<T>().map_err(|_| CompareThresholdParseError)?,
                    ))
                }
                _ => {}
            }
        }
        Err(CompareThresholdParseError)
    }
}
