// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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
        MockMetricsReporter { should_fail: true, metrics: vec![] }
    }
    pub fn new() -> Self {
        MockMetricsReporter { should_fail: false, metrics: vec![] }
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
        let result = mock.report_metrics(Metrics::UpdateCheckResponseTime(Duration::from_secs(2)));
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    fn test_mock_metrics_reporter_error() {
        let mut mock = MockMetricsReporter::new_failing();
        let result = mock.report_metrics(Metrics::UpdateCheckResponseTime(Duration::from_secs(5)));
        assert!(result.is_err());
    }
}
