// Copyright 2024 The Fuchsia Authors
//
// Licensed under a BSD-style license <LICENSE-BSD>, Apache License, Version 2.0
// <LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0>, or the MIT
// license <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your option.
// This file may not be copied, modified, or distributed except according to
// those terms.

use futures::future::BoxFuture;
use futures::prelude::*;
use omaha_client::storage::Storage;
use thiserror::Error;

/// The MinimalStorage return None or Err on everything.
#[derive(Debug)]
pub struct MinimalStorage;

#[derive(Debug, Error)]
pub enum MinimalErrors {
    #[error("MinimalStorage failed intentionally")]
    Intentional,
}

impl Storage for MinimalStorage {
    type Error = MinimalErrors;

    fn get_string<'a>(&'a self, _key: &'a str) -> BoxFuture<'_, Option<String>> {
        future::ready(None).boxed()
    }

    fn get_int<'a>(&'a self, _key: &'a str) -> BoxFuture<'_, Option<i64>> {
        future::ready(None).boxed()
    }

    fn get_bool<'a>(&'a self, _key: &'a str) -> BoxFuture<'_, Option<bool>> {
        future::ready(None).boxed()
    }

    fn set_string<'a>(
        &'a mut self,
        _key: &'a str,
        _value: &'a str,
    ) -> BoxFuture<'_, Result<(), Self::Error>> {
        future::ready(Err(MinimalErrors::Intentional)).boxed()
    }

    fn set_int<'a>(
        &'a mut self,
        _key: &'a str,
        _value: i64,
    ) -> BoxFuture<'_, Result<(), Self::Error>> {
        future::ready(Err(MinimalErrors::Intentional)).boxed()
    }

    fn set_bool<'a>(
        &'a mut self,
        _key: &'a str,
        _value: bool,
    ) -> BoxFuture<'_, Result<(), Self::Error>> {
        future::ready(Err(MinimalErrors::Intentional)).boxed()
    }

    fn remove<'a>(&'a mut self, _key: &'a str) -> BoxFuture<'_, Result<(), Self::Error>> {
        future::ready(Err(MinimalErrors::Intentional)).boxed()
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<(), Self::Error>> {
        future::ready(Err(MinimalErrors::Intentional)).boxed()
    }
}
