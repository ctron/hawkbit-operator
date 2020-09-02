/*
 * Copyright (c) 2020 Red Hat Inc.
 *
 * See the NOTICE file(s) distributed with this work for additional
 * information regarding copyright ownership.
 *
 * This program and the accompanying materials are made available under the
 * terms of the Eclipse Public License 2.0 which is available at
 * http://www.eclipse.org/legal/epl-2.0
 *
 * SPDX-License-Identifier: EPL-2.0
 */

use k8s_openapi::api::core::v1::ResourceRequirements;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube_derive::CustomResource;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::Deref;

#[derive(CustomResource, Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[kube(
    group = "iot.eclipse.org",
    version = "v1alpha1",
    kind = "Hawkbit",
    namespaced,
    derive = "Default",
    derive = "PartialEq",
    status = "HawkbitStatus"
)]
#[kube(apiextensions = "v1")]
#[serde(default, rename_all = "camelCase")]
pub struct HawkbitSpec {
    pub database: Database,
    pub rabbit: Rabbit,
    pub image_overrides: BTreeMap<String, ImageOverride>,
    pub sign_on: Option<SignOn>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SignOn {
    Keycloak {
        #[serde(flatten)]
        config: KeycloakConfig,
    },
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct KeycloakConfig {
    pub use_instance: Option<LabelSelector>,
    pub hawkbit_url: Option<String>,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ImageOverride {
    pub image: Option<String>,
    pub pull_policy: Option<String>,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Database {
    pub mysql: Option<MySQL>,
    pub postgres: Option<PostgreSQL>,
    pub embedded: Option<Embedded>,
}

impl Database {
    pub fn variant(&self) -> anyhow::Result<DatabaseVariant> {
        match &self {
            Database {
                mysql: Some(mysql),
                postgres: None,
                embedded: None,
            } => Ok(DatabaseVariant::MySQL(&mysql)),

            Database {
                mysql: None,
                postgres: Some(postgres),
                embedded: None,
            } => Ok(DatabaseVariant::PostgreSQL(&postgres)),

            Database {
                mysql: None,
                postgres: None,
                embedded: Some(_),
            } => Ok(DatabaseVariant::Embedded),

            Database {
                mysql: _,
                postgres: _,
                embedded: _,
            } => Err(anyhow::anyhow!("Invalid database configuration")),
        }
    }
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Rabbit {
    pub external: Option<RabbitExternal>,
    pub managed: Option<RabbitManaged>,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct RabbitManaged {
    pub storage_size: Option<String>,
    pub resources: Option<ResourceRequirements>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct RabbitExternal {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password_secret: PasswordSecretSource,
}

impl Default for RabbitExternal {
    fn default() -> Self {
        RabbitExternal {
            host: Default::default(),
            port: 5672,
            username: Default::default(),
            password_secret: Default::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct CommonJdbc {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub url: String,

    pub username: String,
    pub password_secret: PasswordSecretSource,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct MySQL {
    #[serde(flatten)]
    pub jdbc: CommonJdbc,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct PostgreSQL {
    #[serde(flatten)]
    pub jdbc: CommonJdbc,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Embedded {}

pub trait DatabaseOptions: Deref<Target = CommonJdbc> {
    fn jdbc_type(&self) -> String;

    fn default_port(&self) -> u16;

    fn url(&self) -> anyhow::Result<String> {
        Ok(if !self.url.is_empty() {
            self.url.clone()
        } else {
            let port = if self.port > 0 {
                self.port
            } else {
                self.default_port()
            };
            format!(
                "jdbc:{}://{}:{}/{}",
                self.jdbc_type(),
                self.host,
                port,
                self.database
            )
        })
    }
}

impl DatabaseOptions for MySQL {
    fn jdbc_type(&self) -> String {
        "mysql".to_string()
    }
    fn default_port(&self) -> u16 {
        3306
    }
}

impl Deref for MySQL {
    type Target = CommonJdbc;

    fn deref(&self) -> &Self::Target {
        &self.jdbc
    }
}

impl DatabaseOptions for PostgreSQL {
    fn jdbc_type(&self) -> String {
        "postgresql".to_string()
    }
    fn default_port(&self) -> u16 {
        5432
    }
}

impl Deref for PostgreSQL {
    type Target = CommonJdbc;

    fn deref(&self) -> &Self::Target {
        &self.jdbc
    }
}

pub enum DatabaseVariant<'a> {
    MySQL(&'a MySQL),
    PostgreSQL(&'a PostgreSQL),
    Embedded,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct PasswordSecretSource {
    pub name: String,
    pub field: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct HawkbitStatus {
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod test {

    use super::*;
    use k8s_openapi::Resource;

    #[test]
    fn verify_resource() {
        assert_eq!(Hawkbit::KIND, "Hawkbit");
        assert_eq!(Hawkbit::GROUP, "iot.eclipse.org");
        assert_eq!(Hawkbit::VERSION, "v1alpha1");
        assert_eq!(Hawkbit::API_VERSION, "iot.eclipse.org/v1alpha1");
    }
}
