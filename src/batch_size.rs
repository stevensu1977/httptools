use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug)]
pub struct BatchSize {
    min: usize,
    max: usize,
}

impl BatchSize {
    pub fn choose(self) -> usize {
        fastrand::usize(self.min..=self.max)
    }

    pub fn max(self) -> usize {
        self.max
    }
}

impl FromStr for BatchSize {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (min, max) = input.split_once('-').unwrap_or((input, input));
        let parse = |s: &str| {
            s.parse::<usize>()
                .map_err(|_| "use N or MIN-MAX, e.g. 1-10".to_string())
        };
        let (min, max) = (parse(min)?, parse(max)?);
        if min == 0 || max < min || max > 100_000 {
            return Err("batch size must satisfy 1 <= MIN <= MAX <= 100000".into());
        }
        Ok(Self { min, max })
    }
}

impl fmt::Display for BatchSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.min == self.max {
            write!(f, "{}", self.min)
        } else {
            write!(f, "{}-{}", self.min, self.max)
        }
    }
}
