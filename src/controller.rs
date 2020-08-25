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

use anyhow::Result;

use crate::crd::{DatabaseOptions, DatabaseVariant, Hawkbit};
use k8s_openapi::api::apps::v1::{Deployment, DeploymentStrategy};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;

use kube::api::{Meta, ObjectMeta, PostParams};
use kube::{Api, Client};

use std::collections::BTreeMap;
use std::fmt::Display;

use operator_framework::install::config::AppendString;
use operator_framework::install::container::{ApplyPort, DropVolume};

use operator_framework::install::container::SetResources;
use operator_framework::install::container::{ApplyContainer, ApplyVolumeMount};
use operator_framework::install::container::{ApplyEnvironmentVariable, ApplyVolume};
use operator_framework::install::meta::OwnedBy;

use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, HTTPGetAction, PersistentVolumeClaim,
    PersistentVolumeClaimVolumeSource, Probe, Secret, Service, ServiceAccount, ServicePort,
};
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

use openshift_openapi::api::route::v1::{Route, RoutePort, TLSConfig};

use operator_framework::process::create_or_update;
use operator_framework::tracker::{ConfigTracker, Trackable};
use operator_framework::utils::UseOrCreate;

use passwords::PasswordGenerator;

pub struct HawkbitController {
    client: Client,
    deployments: Api<Deployment>,
    secrets: Api<Secret>,
    configmaps: Api<ConfigMap>,
    service_accounts: Api<ServiceAccount>,
    pvcs: Api<PersistentVolumeClaim>,
    roles: Api<Role>,
    role_bindings: Api<RoleBinding>,
    services: Api<Service>,
    routes: Option<Api<Route>>,

    passwords: PasswordGenerator,
}

pub const HAWKBIT_REGISTRY: &str = "docker.io/hawkbit";
pub const HAWKBIT_VERSION: &str = "0.3.0M6";

pub const KUBERNETES_LABEL_NAME: &str = "app.kubernetes.io/name";
pub const KUBERNETES_LABEL_INSTANCE: &str = "app.kubernetes.io/instance";
pub const KUBERNETES_LABEL_COMPONENT: &str = "app.kubernetes.io/component";
pub const OPENSHIFT_ANNOTATION_CONNECT: &str = "app.openshift.io/connects-to";

impl HawkbitController {
    pub fn new(namespace: &str, client: Client, has_openshift: bool) -> Self {
        HawkbitController {
            client: client.clone(),
            deployments: Api::namespaced(client.clone(), &namespace),
            secrets: Api::namespaced(client.clone(), &namespace),
            service_accounts: Api::namespaced(client.clone(), &namespace),
            roles: Api::namespaced(client.clone(), &namespace),
            role_bindings: Api::namespaced(client.clone(), &namespace),
            services: Api::namespaced(client.clone(), &namespace),
            configmaps: Api::namespaced(client.clone(), &namespace),
            pvcs: Api::namespaced(client.clone(), &namespace),
            routes: if has_openshift {
                Some(Api::namespaced(client.clone(), &namespace))
            } else {
                None
            },

            passwords: PasswordGenerator {
                length: 16,
                ..Default::default()
            },
        }
    }

    fn image_name<S>(&self, base: S) -> String
    where
        S: ToString + Display,
    {
        format!("{}/{}:{}", HAWKBIT_REGISTRY, base, HAWKBIT_VERSION)
    }

    pub async fn reconcile(&self, original: &Hawkbit) -> Result<()> {
        let resource = original.clone();
        let namespace = resource.namespace().expect("Missing namespace");

        let result = self.do_reconcile(resource).await;

        let resource = match result {
            Ok(mut resource) => {
                resource.status.use_or_create(|status| {
                    status.phase = "Active".into();
                    status.message = None;
                });
                resource
            }
            Err(err) => {
                let mut resource = original.clone();
                resource.status.use_or_create(|status| {
                    status.phase = "Failed".into();
                    status.message = Some(err.to_string());
                });
                resource
            }
        };

        if !original.eq(&resource) {
            Api::<Hawkbit>::namespaced(self.client.clone(), &namespace)
                .replace_status(
                    &resource.name(),
                    &PostParams::default(),
                    serde_json::to_vec(&resource)?,
                )
                .await?;
        }

        Ok(())
    }

    async fn do_reconcile(&self, resource: Hawkbit) -> Result<Hawkbit> {
        let prefix = resource.name();
        let namespace = resource.namespace().expect("Missing namespace");

        log::info!("Reconcile: {}/{}", namespace, prefix);

        let mut config_tracker = &mut ConfigTracker::new();

        self.create_service_account(&resource, &namespace).await?;
        self.create_application_config(&resource, &namespace, &mut config_tracker)
            .await?;
        self.create_application_secrets(&resource, &namespace, &mut config_tracker)
            .await?;
        self.create_storage(&resource, &namespace).await?;

        self.create_deployment(&resource, &namespace, &config_tracker)
            .await?;
        self.create_service(&resource, &namespace).await?;

        if let Some(routes) = &self.routes {
            self.create_route(routes, &resource, &namespace).await?;
        }

        Ok(resource)
    }

    async fn create_application_config(
        &self,
        resource: &Hawkbit,
        namespace: &String,
        tracker: &mut ConfigTracker,
    ) -> Result<()> {
        create_or_update(
            &self.configmaps,
            Some(namespace),
            resource.name(),
            |mut cm| {
                cm.owned_by_controller(resource)?;
                let cfg = r#"server:
  useForwardHeaders: true
hawkbit: null
spring:
  cloud:
    stream:
      bindings:
        default:
          group: hawkbit
        device-created:
          destination: device-registry.device-created
        device-deleted:
          destination: device-registry.device-deleted
        device-updated:
          destination: device-registry.device-updated
"#
                .to_string();

                cm.append_string("application.yaml", cfg);
                cm.track_with(tracker);

                Ok(cm)
            },
        )
        .await?;

        Ok(())
    }

    async fn create_application_secrets(
        &self,
        resource: &Hawkbit,
        namespace: &String,
        tracker: &mut ConfigTracker,
    ) -> Result<()> {
        create_or_update(
            &self.secrets,
            Some(namespace),
            format!("{}-admin", resource.name()),
            |mut secret| {
                secret.owned_by_controller(resource)?;

                secret.init_string("adminUsername", "admin");
                secret.init_string_from("adminPassword", || {
                    let pwd = self.passwords.generate_one().unwrap();
                    format!("{{noop}}{}", pwd)
                });

                secret.track_with(tracker);

                Ok(secret)
            },
        )
        .await?;

        Ok(())
    }

    async fn create_service_account(&self, resource: &Hawkbit, namespace: &String) -> Result<()> {
        create_or_update(
            &self.service_accounts,
            Some(namespace),
            resource.name(),
            |mut sa| {
                sa.owned_by_controller(resource)?;
                Ok(sa)
            },
        )
        .await?;

        Ok(())
    }

    async fn create_storage(&self, resource: &Hawkbit, namespace: &String) -> Result<()> {
        create_or_update(
            &self.pvcs,
            Some(namespace),
            format!("{}-data", resource.name()),
            |mut pvc| {
                pvc.owned_by_controller(resource)?;
                pvc.spec.use_or_create(|spec| {
                    spec.access_modes = Some(vec!["ReadWriteOnce".into()]);
                    spec.resources.use_or_create(|resources| {
                        resources.set_resources::<&str, &str, &str>(
                            "storage".into(),
                            Some("1Gi"),
                            None,
                        );
                    });
                });
                Ok(pvc)
            },
        )
        .await?;

        match resource.spec.database.variant()? {
            DatabaseVariant::Embedded => {
                create_or_update(
                    &self.pvcs,
                    Some(namespace),
                    format!("{}-embedded-db", resource.name()),
                    |mut pvc| {
                        pvc.owned_by_controller(resource)?;
                        pvc.spec.use_or_create(|spec| {
                            spec.access_modes = Some(vec!["ReadWriteOnce".into()]);
                            spec.resources.use_or_create(|resources| {
                                resources.set_resources::<&str, &str, &str>(
                                    "storage".into(),
                                    Some("1Gi"),
                                    None,
                                );
                            });
                        });
                        Ok(pvc)
                    },
                )
                .await?;
            }
            _ => {}
        }

        Ok(())
    }

    fn apply_common_jdbc<J>(&self, container: &mut Container, jdbc: &J) -> Result<()>
    where
        J: DatabaseOptions,
    {
        container.add_env("SPRING_DATASOURCE_URL", jdbc.url()?)?;

        container.add_env("SPRING_DATASOURCE_USERNAME", &jdbc.username)?;
        container.add_env_from_secret(
            "SPRING_DATASOURCE_PASSWORD",
            &jdbc.password_secret.name,
            &jdbc.password_secret.field,
        )?;

        // done

        Ok(())
    }

    async fn create_deployment(
        &self,
        resource: &Hawkbit,
        namespace: &String,
        tracker: &ConfigTracker,
    ) -> Result<()> {
        let mut selector = self.selector(resource, "server", "server");

        create_or_update(
            &self.deployments,
            Some(namespace),
            resource.name(),
            |mut deployment| {
                deployment.owned_by_controller(resource)?;

                deployment.metadata.labels = Some(selector.clone());

                deployment.spec.use_or_create_err(|mut spec| {
                    // always scale to 1
                    spec.replicas = Some(1);

                    spec.strategy = Some(DeploymentStrategy {
                        type_: Some("Recreate".into()),
                        ..Default::default()
                    });

                    spec.selector = LabelSelector {
                        match_labels: Some(selector.clone()),
                        ..Default::default()
                    };

                    selector.insert("config-hash".into(), tracker.current_hash());

                    spec.template.metadata = Some(ObjectMeta {
                        labels: Some(selector.clone()),
                        ..Default::default()
                    });

                    // containers

                    spec.template.apply_container("server", |container| {
                        let over = resource.spec.image_overrides.get("hawkbit-update-server");

                        let suffix = match &resource.spec.database.variant()? {
                            DatabaseVariant::MySQL(_) => "-mysql",
                            _ => "",
                        };

                        let image_name = over
                            .and_then(|o| o.image.clone())
                            .unwrap_or_else(|| self.image_name("hawkbit-update-server") + suffix);

                        container.image = Some(image_name);
                        container.image_pull_policy = over.and_then(|o| o.pull_policy.clone());

                        container.args = None;
                        container.command = Some(
                            vec!["java", "-jar", "hawkbit-update-server.jar"]
                                .iter()
                                .map(|s| s.to_string())
                                .collect(),
                        );
                        container.working_dir = None;

                        container.env = Some(vec![]);

                        match resource.spec.database.variant()? {
                            DatabaseVariant::MySQL(mysql) => {
                                container.add_env("SPRING_JPA_DATABASE", "MYSQL")?;
                                /*
                                container.add_env(
                                    "SPRING_DATASOURCE_DRIVERCLASSNAME",
                                    "com.mysql.jdbc.Driver",
                                )?;
                                 */
                                // FIXME: once we have Spring Boot 2.2 and can drop in JDBC drivers
                                container.add_env(
                                    "SPRING_DATASOURCE_DRIVERCLASSNAME",
                                    "org.mariadb.jdbc.Driver",
                                )?;
                                self.apply_common_jdbc(container, mysql)?;
                            }
                            DatabaseVariant::PostgreSQL(postgres) => {
                                container.add_env("SPRING_JPA_DATABASE", "POSTGRESQL")?;
                                container.add_env(
                                    "SPRING_DATASOURCE_DRIVERCLASSNAME",
                                    "org.postgresql.Driver",
                                )?;
                                self.apply_common_jdbc(container, postgres)?;
                            }
                            DatabaseVariant::Embedded => {
                                container.add_env("SPRING_JPA_DATABASE", "H2")?;
                                container.add_env(
                                    "SPRING_DATASOURCE_DRIVERCLASSNAME",
                                    "org.h2.Driver",
                                )?;
                                container.add_env(
                                    "SPRING_DATASOURCE_URL",
                                    "jdbc:h2:/embedded-db/hawkbit",
                                )?;

                                container.apply_volume_mount_simple(
                                    "embedded-db",
                                    "/embedded-db",
                                    false,
                                )?;
                            }
                        }

                        let secret_name = format!("{}-admin", resource.name());
                        container.add_env_from_secret(
                            "SPRING_SECURITY_USER_NAME",
                            &secret_name,
                            "adminUsername",
                        )?;
                        container.add_env_from_secret(
                            "SPRING_SECURITY_USER_PASSWORD",
                            &secret_name,
                            "adminPassword",
                        )?;

                        container.add_env("SPRING_RABBITMQ_HOST", &resource.spec.rabbit.host)?;
                        container.add_env(
                            "SPRING_RABBITMQ_PORT",
                            format!("{}", resource.spec.rabbit.port),
                        )?;
                        container
                            .add_env("SPRING_RABBITMQ_USERNAME", &resource.spec.rabbit.username)?;
                        container.add_env_from_secret(
                            "SPRING_RABBITMQ_PASSWORD",
                            &resource.spec.rabbit.password_secret.name,
                            &resource.spec.rabbit.password_secret.field,
                        )?;

                        container.add_env(
                            "org.eclipse.hawkbit.repository.file.path",
                            "/etc/hawkbit/storage",
                        )?;

                        if self.routes.is_some() {
                            container.add_env(
                                "hawkbit.artifact.url.protocols.download-http.port",
                                "443",
                            )?;
                            container.add_env(
                                "hawkbit.artifact.url.protocols.download-http.protocol",
                                "https",
                            )?;
                        }

                        container.add_port("http", 8080, Some("TCP".into()))?;

                        container.readiness_probe = Some(Probe {
                            failure_threshold: Some(3),
                            http_get: Some(HTTPGetAction {
                                path: Some("/VAADIN/themes/hawkbit/favicon.ico".into()),
                                port: IntOrString::String("http".into()),
                                ..Default::default()
                            }),
                            initial_delay_seconds: Some(10),
                            period_seconds: Some(1),
                            success_threshold: Some(1),
                            timeout_seconds: Some(2),
                            ..Default::default()
                        });
                        container.liveness_probe = Some(Probe {
                            failure_threshold: Some(5),
                            http_get: Some(HTTPGetAction {
                                path: Some("/VAADIN/themes/hawkbit/favicon.ico".into()),
                                port: IntOrString::String("http".into()),
                                ..Default::default()
                            }),
                            initial_delay_seconds: Some(30),
                            period_seconds: Some(5),
                            success_threshold: Some(1),
                            timeout_seconds: Some(2),
                            ..Default::default()
                        });

                        container.apply_volume_mount_simple(
                            "config",
                            "/opt/hawkbit/config",
                            true,
                        )?;
                        container.apply_volume_mount_simple(
                            "storage",
                            "/etc/hawkbit/storage",
                            false,
                        )?;

                        Ok(())
                    })?;

                    spec.template.spec.use_or_create_err(|pod_spec| {
                        pod_spec.service_account = Some(resource.name());
                        pod_spec.init_containers = None;
                        pod_spec.apply_volume("config", |volume| {
                            volume.config_map = Some(ConfigMapVolumeSource {
                                name: Some(resource.name().into()),
                                ..Default::default()
                            });
                            Ok(())
                        })?;

                        pod_spec.apply_volume("storage", |volume| {
                            volume.persistent_volume_claim =
                                Some(PersistentVolumeClaimVolumeSource {
                                    claim_name: format!("{}-data", resource.name()),
                                    ..Default::default()
                                });
                            Ok(())
                        })?;

                        match resource.spec.database.variant()? {
                            DatabaseVariant::Embedded => {
                                pod_spec.apply_volume("embedded-db", |volume| {
                                    volume.persistent_volume_claim =
                                        Some(PersistentVolumeClaimVolumeSource {
                                            claim_name: format!("{}-embedded-db", resource.name()),
                                            ..Default::default()
                                        });
                                    Ok(())
                                })?;
                            }
                            _ => {
                                pod_spec.drop_volume("embedded-db");
                            }
                        }

                        Ok(())
                    })?;

                    Ok(())
                })?;

                log::debug!("Deployment: {:#?}", &deployment);

                Ok(deployment)
            },
        )
        .await?;

        Ok(())
    }

    fn selector<S1, S2>(
        &self,
        resource: &Hawkbit,
        name: S1,
        component: S2,
    ) -> BTreeMap<String, String>
    where
        S1: ToString,
        S2: ToString,
    {
        let prefix = resource.name();

        let name = name.to_string();

        let mut selector = BTreeMap::new();
        selector.insert(KUBERNETES_LABEL_NAME.into(), name.clone());
        selector.insert(
            KUBERNETES_LABEL_INSTANCE.into(),
            format!("{}-{}", name, &prefix),
        );
        selector.insert(KUBERNETES_LABEL_COMPONENT.into(), component.to_string());

        selector
    }

    async fn create_service(&self, resource: &Hawkbit, namespace: &String) -> Result<()> {
        let selector = self.selector(resource, "server", "server");

        create_or_update(
            &self.services,
            Some(namespace),
            resource.name(),
            |mut service| {
                service.owned_by_controller(resource)?;

                service.metadata.labels = Some(selector.clone());

                service.spec.use_or_create(|spec| {
                    spec.type_ = Some("ClusterIP".into());
                    spec.ports = Some(vec![ServicePort {
                        name: Some("http".into()),
                        port: 8080,
                        protocol: Some("TCP".into()),
                        target_port: Some(IntOrString::String("http".into())),
                        ..Default::default()
                    }]);
                    spec.selector = Some(selector);
                });

                Ok(service)
            },
        )
        .await?;

        Ok(())
    }

    async fn create_route(
        &self,
        routes: &Api<Route>,
        resource: &Hawkbit,
        namespace: &String,
    ) -> Result<()> {
        let selector = self.selector(resource, "server", "server");

        create_or_update(routes, Some(namespace), resource.name(), |mut route| {
            route.owned_by_controller(resource)?;

            route.metadata.labels = Some(selector.clone());

            route.spec.to.name = resource.name();
            route.spec.to.kind = "Service".into();
            route.spec.to.weight = 100;
            route.spec.port = Some(RoutePort {
                target_port: IntOrString::String("http".into()),
            });

            route.spec.tls = Some(TLSConfig {
                insecure_edge_termination_policy: Some("None".into()),
                termination: "edge".into(),
                ..Default::default()
            });

            Ok(route)
        })
        .await?;

        Ok(())
    }
}
