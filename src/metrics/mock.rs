// Copyright 2019 The Fuchsia Authors
//
// Licensed under a BSD-style license <LICENSE-BSD>, Apache License, Version 2.0
// <LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0>, or the MIT
// license <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your option.
// This file may not be copied, modified, or distributed except according to
// those terms.

use crate::metrics::{Metrics, MetricsReporter};
use anyhow::{format_err, Error};

/// A mock implementation of MetricsReporter save the metrics into a vector.
#[derive(Debug)]
pub struct MockMetricsReporter {
    pub should_fail: bool,
    pub metrics: Vec<Metrics>,
}

impl MockMetricsReporter {
    pub fn new_failing() -> Self {
        MockMetricsReporter {
            should_fail: true,
            metrics: vec![],
        }
    }

    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        MockMetricsReporter {
            should_fail: false,
            metrics: vec![],
        }
    }
}

impl MetricsReporter for MockMetricsReporter {
    fn report_metrics(&mut self, metrics: Metrics) -> Result<(), Error> {
        self.metrics.push(metrics);
        if self.should_fail {
            Err(format_err!("should_fail is true"))
        } else {
            Ok(())
        }
    }
}

mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_mock_metrics_reporter() {
        let mut mock = MockMetricsReporter::new();
        let result = mock.report_metrics(Metrics::UpdateCheckResponseTime {
            response_time: Duration::from_secs(2),
            successful: true,
        });
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn test_mock_metrics_reporter_error() {
        let mut mock = MockMetricsReporter::new_failing();
        let result = mock.report_metrics(Metrics::UpdateCheckResponseTime {
            response_time: Duration::from_secs(5),
            successful: true,
        });
        assert!(result.is_err());
    }
}
