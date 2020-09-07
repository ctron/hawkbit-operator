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

// required because of kube-runtime
#![type_length_limit = "14255639"]

mod controller;
mod crd;

use kube::api::ListParams;
use kube::{Api, Client};
use kube_runtime::Controller;

use crd::Hawkbit;

use crate::controller::HawkbitController;
use futures::StreamExt;
use futures::TryFutureExt;

use snafu::Snafu;
use std::fmt;

use keycloak_crd::{Keycloak, KeycloakClient, KeycloakRealm, KeycloakUser};

#[derive(Debug, Snafu)]
enum ReconcileError {
    ControllerError { source: anyhow::Error },
}

use k8s_openapi::api::apps::v1::{Deployment, StatefulSet};
use k8s_openapi::api::core::v1::{
    ConfigMap, PersistentVolumeClaim, Secret, Service, ServiceAccount,
};
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use kube_runtime::controller::{Context, ReconcilerAction};
use openshift_openapi::api::route::v1::Route;
use std::error::Error;
use tokio::time::Duration;

#[derive(Debug, Clone)]
struct StringError {
    message: String,
}

impl Error for StringError {}

impl fmt::Display for StringError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", &self.message)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let client = Client::try_default().await?;
    let namespace = std::env::var("NAMESPACE").unwrap_or("default".into());

    let has_openshift = std::env::var_os("HAS_OPENSHIFT")
        .map(|s| s.into_string())
        .transpose()
        .map_err(|err| StringError {
            message: err.to_string_lossy().into(),
        })?
        .map_or(false, |s| s == "true");

    let controller = HawkbitController::new(&namespace, client.clone(), has_openshift);
    let context = Context::new(());

    log::info!("Starting operator...");

    let hawkbits: Api<Hawkbit> = Api::namespaced(client.clone(), &namespace);
    let c = Controller::new(hawkbits, ListParams::default())
        .owns(
            Api::<ConfigMap>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<Deployment>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<PersistentVolumeClaim>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<Role>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<RoleBinding>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<Secret>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<Service>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<ServiceAccount>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<StatefulSet>::namespaced(client.clone(), &namespace),
            Default::default(),
        );

    // watch keycloak

    let c = c
        .owns(
            Api::<Keycloak>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<KeycloakRealm>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<KeycloakClient>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
        .owns(
            Api::<KeycloakUser>::namespaced(client.clone(), &namespace),
            Default::default(),
        );

    // watch openshift resources as well

    let c = if has_openshift {
        c.owns(
            Api::<Route>::namespaced(client.clone(), &namespace),
            Default::default(),
        )
    } else {
        c
    };

    // FIXME: need to watch references secrets as well

    // now run it

    c.run(
        |resource, _| {
            controller
                .reconcile(resource)
                .map_ok(|_| ReconcilerAction {
                    requeue_after: Some(Duration::from_secs(600)),
                })
                .map_err(|err| ReconcileError::ControllerError { source: err })
        },
        |_, _| ReconcilerAction {
            requeue_after: Some(Duration::from_secs(600)),
        },
        context,
    )
    // the next two lines are required to poll from the stream
    .for_each(|res| async move {
        match res {
            Ok(o) => log::debug!("reconciled {:?}", o),
            Err(e) => log::info!("reconcile failed: {:?}", e),
        }
    })
    .await;

    Ok(())
}
