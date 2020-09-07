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

mod keycloak;

use anyhow::Result;

use crate::crd::{DatabaseOptions, DatabaseVariant, Hawkbit, KeycloakConfig};
use k8s_openapi::api::apps::v1::{
    Deployment, DeploymentStrategy, StatefulSet, StatefulSetUpdateStrategy,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;

use kube::api::{DeleteParams, Meta, ObjectMeta, PostParams};
use kube::{Api, Client};

use std::collections::BTreeMap;
use std::fmt::Display;

use operator_framework::install::config::AppendString;
use operator_framework::install::container::{ApplyPort, DropVolume, DropVolumeMount};

use operator_framework::install::container::SetResources;
use operator_framework::install::container::{ApplyContainer, ApplyVolumeMount};
use operator_framework::install::container::{ApplyEnvironmentVariable, ApplyVolume};
use operator_framework::install::meta::OwnedBy;

use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, EmptyDirVolumeSource, ExecAction, HTTPGetAction,
    PersistentVolumeClaim, PersistentVolumeClaimSpec, PersistentVolumeClaimVolumeSource, Probe,
    ResourceRequirements, Secret, Service, ServiceAccount, ServicePort,
};
use k8s_openapi::api::rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

use openshift_openapi::api::route::v1::{Route, RoutePort, TLSConfig};

use operator_framework::process::create_or_update;
use operator_framework::tracker::{ConfigTracker, Trackable};
use operator_framework::utils::UseOrCreate;

use anyhow::anyhow;

use crate::crd::SignOn;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::PropagationPolicy::Background;
use operator_framework::install::DeleteOptionally;
use passwords::PasswordGenerator;
use std::iter::FromIterator;

use crate::controller::keycloak::all_roles;
use keycloak_crd::{Credential, Keycloak, KeycloakClient, KeycloakRealm, KeycloakUser};
use kube_runtime::controller::ReconcilerAction;
use tokio::time::Duration;

pub struct HawkbitController {
    client: Client,

    configmaps: Api<ConfigMap>,
    deployments: Api<Deployment>,
    pvcs: Api<PersistentVolumeClaim>,
    roles: Api<Role>,
    role_bindings: Api<RoleBinding>,
    secrets: Api<Secret>,
    services: Api<Service>,
    service_accounts: Api<ServiceAccount>,
    statefulsets: Api<StatefulSet>,

    routes: Option<Api<Route>>,

    keycloak: Api<Keycloak>,
    keycloak_realms: Api<KeycloakRealm>,
    keycloak_clients: Api<KeycloakClient>,
    keycloak_users: Api<KeycloakUser>,

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
            statefulsets: Api::namespaced(client.clone(), &namespace),
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

            keycloak: Api::namespaced(client.clone(), &namespace),
            keycloak_clients: Api::namespaced(client.clone(), &namespace),
            keycloak_realms: Api::namespaced(client.clone(), &namespace),
            keycloak_users: Api::namespaced(client.clone(), &namespace),

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

    pub async fn reconcile(&self, original: Hawkbit) -> Result<()> {
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
                log::info!("Failed to reconcile: {}", err);
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

        // route first, as we need the URL

        let url = if let Some(routes) = &self.routes {
            self.create_route(routes, &resource, &namespace).await?
        } else {
            None
        };

        // reconcile rabbitmq embedded

        match &resource.spec.rabbit.managed {
            Some(_) => self.deploy_managed_rabbit(&resource).await?,
            _ => self.delete_managed_rabbit(&resource).await?,
        }

        // reconcile keycloak

        let issuer_uri = match &resource.spec.sign_on {
            Some(SignOn::Keycloak { config }) => {
                self.deploy_keycloak(&resource, &config, &url).await?
            }
            _ => None,
        };

        // reconcile hawkbit update server

        let mut config_tracker = &mut ConfigTracker::new();

        self.create_service_account(&resource, &namespace).await?;
        self.create_application_config(&resource, &namespace, &mut config_tracker, issuer_uri)
            .await?;
        self.create_application_secrets(&resource, &namespace, &mut config_tracker)
            .await?;
        self.create_storage(&resource, &namespace).await?;

        self.create_deployment(&resource, &namespace, &config_tracker)
            .await?;
        self.create_service(&resource, &namespace).await?;

        Ok(resource)
    }

    async fn deploy_keycloak(
        &self,
        resource: &Hawkbit,
        keycloak: &KeycloakConfig,
        hawkbit_url: &Option<String>,
    ) -> Result<Option<String>> {
        let instance_labels = self.selector(&resource, "keycloak", "sso");
        let realm_labels = self.selector(&resource, "keycloak-realm", "sso");
        let client_labels = self.selector(&resource, "keycloak-client", "sso");

        let hawkbit_url = keycloak.hawkbit_url.as_ref().or(hawkbit_url.as_ref());
        let hawkbit_url = match (hawkbit_url, self.routes.is_some()) {
            (None, true) => Err(anyhow!("Waiting for hawkbit route to be applied")),
            (None, false) => Err(anyhow!("Missing '.spec.signOn.keycloak.hawkbitUrl'")),
            (Some(url), _) => Ok(url),
        }?;

        create_or_update(
            &self.keycloak,
            resource.namespace(),
            resource.name(),
            |mut instance| {
                instance.owned_by_controller(resource)?;

                instance.metadata.labels = Some(instance_labels.clone());

                instance.spec.instances = 1;
                instance.spec.external_access.enabled = true;

                Ok(instance)
            },
        )
        .await?;

        create_or_update(
            &self.keycloak_realms,
            resource.namespace(),
            format!("{}-hawkbit", resource.name()),
            |mut realm| {
                realm.owned_by_controller(resource)?;

                realm.metadata.labels = Some(realm_labels.clone());

                realm.spec.instance_selector = LabelSelector {
                    match_labels: Some(instance_labels.clone()),
                    ..Default::default()
                };

                realm.spec.realm.display_name = "hawkBit Realm".into();
                realm.spec.realm.enabled = true;
                realm.spec.realm.realm = format!("{}-hawkbit", resource.name());

                Ok(realm)
            },
        )
        .await?;

        create_or_update(
            &self.keycloak_clients,
            resource.namespace(),
            format!("{}-hawkbit", resource.name()),
            |mut client| {
                client.owned_by_controller(resource)?;

                client.metadata.labels = Some(client_labels.clone());

                client.spec.realm_selector = LabelSelector {
                    match_labels: Some(realm_labels.clone()),
                    ..Default::default()
                };

                client.spec.client.client_id = format!("{}-hawkbit", resource.name());
                client.spec.client.enabled = true;
                client.spec.client.root_url = "".into();
                client.spec.client.base_url = hawkbit_url.clone();
                client.spec.client.redirect_uris =
                    vec![format!("{}/login/oauth2/code/hawkbit", hawkbit_url)];
                client.spec.client.client_authenticator_type = "client-secret".into();
                client.spec.client.implicit_flow_enabled = false;
                client.spec.client.public_client = false;
                client.spec.client.standard_flow_enabled = true;

                // client roles

                client.spec.client.default_roles = all_roles();

                // done

                Ok(client)
            },
        )
        .await?;

        create_or_update(
            &self.keycloak_users,
            resource.namespace(),
            format!("{}-admin", resource.name()),
            |mut user| {
                user.owned_by_controller(resource)?;

                user.spec.realm_selector = LabelSelector {
                    match_labels: Some(realm_labels),
                    ..Default::default()
                };

                // if we don't any credentials ...
                if user.spec.user.credentials.is_empty() {
                    // ... create a password
                    let password = self.passwords.generate_one().map_err(|err| anyhow!(err))?;
                    let cred = Credential {
                        temporary: false,
                        r#type: "password".to_string(),
                        value: password,
                    };
                    user.spec.user.credentials.push(cred);
                }

                user.spec.user.username = "admin".into();
                user.spec.user.enabled = true;
                user.spec.user.first_name = "System".into();
                user.spec.user.last_name = "Admin".into();
                user.spec.user.client_roles.insert(
                    format!("{}-hawkbit", resource.name()),
                    keycloak::all_roles(),
                );

                Ok(user)
            },
        )
        .await?;

        keycloak::find_issuer_uri(
            &self.keycloak,
            format!("{}-hawkbit", resource.name()),
            &instance_labels,
        )
        .await
    }

    async fn deploy_managed_rabbit(&self, resource: &Hawkbit) -> Result<()> {
        let base = format!("{}-rabbit", &resource.name());
        let config_tracker = &mut ConfigTracker::new();

        let selector = self.selector(&resource, "rabbit", "broker");

        create_or_update(
            &self.service_accounts,
            resource.namespace(),
            &base,
            |mut sa| {
                sa.owned_by_controller(resource)?;
                sa.metadata.labels = Some(selector.clone());
                Ok(sa)
            },
        )
        .await?;

        create_or_update(&self.roles, resource.namespace(), &base, |mut role| {
            role.owned_by_controller(resource)?;
            role.metadata.labels = Some(selector.clone());

            role.rules = Some(vec![
                PolicyRule {
                    api_groups: Some(vec!["".into()]),
                    resources: Some(vec!["endpoints".into()]),
                    verbs: vec!["get".into(), "list".into(), "watch".into()],
                    ..Default::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["".into()]),
                    resources: Some(vec!["events".into()]),
                    verbs: vec!["create".into()],
                    ..Default::default()
                },
            ]);

            Ok(role)
        })
        .await?;

        create_or_update(
            &self.role_bindings,
            resource.namespace(),
            &base,
            |mut rb| {
                rb.owned_by_controller(resource)?;
                rb.metadata.labels = Some(selector.clone());

                rb.subjects = Some(vec![Subject {
                    kind: "ServiceAccount".into(),
                    name: base.clone(),
                    ..Default::default()
                }]);

                rb.role_ref = RoleRef {
                    api_group: "rbac.authorization.k8s.io".into(),
                    kind: "Role".into(),
                    name: base.clone(),
                };

                Ok(rb)
            },
        )
        .await?;

        create_or_update(&self.secrets, resource.namespace(), &base, |mut secret| {
            secret.owned_by_controller(resource)?;
            secret.metadata.labels = Some(selector.clone());

            secret.init_string_from("erlang.cookie", || "1234");
            secret.init_string_from("default.username", || "hawkbit");
            secret.init_string_from("default.password", || "hawkbit");
            secret.track_with(config_tracker);

            Ok(secret)
        })
        .await?;

        create_or_update(&self.configmaps, resource.namespace(), &base, |mut cm| {
            cm.owned_by_controller(resource)?;
            cm.metadata.labels = Some(selector.clone());

            cm.append_string(
                "rabbitmq.conf",
                format!(
                    r#"# Rabbit configuration

cluster_formation.peer_discovery_backend = rabbit_peer_discovery_k8s
cluster_formation.k8s.address_type = hostname
cluster_formation.node_cleanup.interval = 10
cluster_formation.node_cleanup.only_log_warning = true
cluster_partition_handling = autoheal

loopback_users.guest = false

queue_master_locator = min-masters
"#
                ),
            );

            cm.append_string(
                "enabled_plugins",
                "[rabbitmq_management, rabbitmq_peer_discovery_k8s].",
            );

            cm.track_with(config_tracker);

            Ok(cm)
        })
        .await?;

        create_or_update(
            &self.statefulsets,
            resource.namespace(),
            &base,
            |mut statefulset| {
                statefulset.owned_by_controller(resource)?;
                statefulset.metadata.labels = Some(selector.clone());

                statefulset.spec.use_or_create_err(|mut spec| {
                    spec.service_name = format!("{}-headless", &base);

                    spec.selector = LabelSelector {
                        match_labels: Some(selector.clone()),
                        ..Default::default()
                    };

                    spec.template.metadata.use_or_create(|meta| {
                        meta.labels = Some(selector.clone());
                        meta.annotations.use_or_create(|annotations| {
                            annotations.insert("config-hash".into(), config_tracker.current_hash());
                        });
                    });

                    spec.pod_management_policy = Some("OrderedReady".into());
                    spec.update_strategy = Some(StatefulSetUpdateStrategy {
                        type_: Some("RollingUpdate".into()),
                        ..Default::default()
                    });

                    spec.template.spec.use_or_create_err(|pod_spec| {
                        pod_spec.service_account = Some(base.clone());
                        pod_spec.restart_policy = Some("Always".into());

                        pod_spec.init_containers = None;

                        pod_spec.apply_volume("config", |volume| {
                            volume.config_map = Some(ConfigMapVolumeSource {
                                name: Some(base.clone()),
                                default_mode: Some(420),
                                ..Default::default()
                            });
                            Ok(())
                        })?;
                        pod_spec.apply_volume("config-rw", |volume| {
                            volume.empty_dir = Some(EmptyDirVolumeSource { ..Default::default() });
                            Ok(())
                        })?;

                        pod_spec.init_containers.apply_container("init-config", |container|{
                            container.image = Some("registry.access.redhat.com/ubi8-minimal".into());

                            container.add_env_from_secret(
                                "RABBITMQ_DEFAULT_USER",
                                &base,
                                "default.username",
                            )?;
                            container.add_env_from_secret(
                                "RABBITMQ_DEFAULT_PASSWORD",
                                &base,
                                "default.password",
                            )?;

                            container.apply_volume_mount_simple("config", "/etc/config", false)?;
                            container.apply_volume_mount_simple("config-rw", "/etc/config-rw", false)?;

                            container.command = Some(vec!["bash", "-c", r#"
set -e

cp /etc/config/* /etc/config-rw/
echo "default_user = ${RABBITMQ_DEFAULT_USER}" >> /etc/config-rw/rabbitmq.conf
echo "default_pass = ${RABBITMQ_DEFAULT_PASSWORD}" >> /etc/config-rw/rabbitmq.conf

"#]
                                .iter()
                                .map(|s| s.to_string())
                                .collect()
                            );

                            Ok(())
                        })?;

                        pod_spec.containers.apply_container("rabbit", |container| {
                            container.image = Some("docker.io/library/rabbitmq:3".into());

                            container.drop_volume_mount("config");
                            container.apply_volume_mount_simple("config-rw", "/etc/rabbitmq", false)?;
                            container.apply_volume_mount_simple(
                                "storage",
                                "/var/lib/rabbitmq/mnesia",
                                false,
                            )?;

                            container.env = Some(vec![]);
                            container.add_env_from_secret(
                                "RABBITMQ_ERLANG_COOKIE",
                                &base,
                                "erlang.cookie",
                            )?;

                            container.add_env_from_field_path("MY_POD_IP", "status.podIP")?;
                            container.add_env_from_field_path("MY_POD_NAME", "metadata.name")?;
                            container.add_env_from_field_path(
                                "MY_POD_NAMESPACE",
                                "metadata.namespace",
                            )?;

                            container.add_env("RABBITMQ_FORCE_BOOT", "false")?;
                            container.add_env("K8S_SERVICE_NAME", format!("{}-headless", base))?;
                            container.add_env("K8S_HOSTNAME_SUFFIX", ".$(K8S_SERVICE_NAME).$(MY_POD_NAMESPACE).svc.cluster.local")?;

                            container.add_env("RABBITMQ_NODE_NAME", "rabbit@$(MY_POD_NAME).$(K8S_SERVICE_NAME).$(MY_POD_NAMESPACE).svc.cluster.local")?;
                            container.add_env("RABBITMQ_NODENAME", "rabbit@$(MY_POD_NAME).$(K8S_SERVICE_NAME).$(MY_POD_NAMESPACE).svc.cluster.local")?;
                            container.add_env("RABBITMQ_USE_LONGNAME", "true")?;

                            // ports

                            container.add_port("amqp", 5672, Some("TCP".into()))?;
                            container.add_port("management", 15672, Some("TCP".into()))?;
                            container.add_port("epmd", 4369, Some("TCP".into()))?;
                            container.add_port("cluster-links", 25672, Some("TCP".into()))?;

                            // checks

                            container.liveness_probe = Some(Probe {
                                exec: Some(ExecAction {
                                    command: Some(vec!["rabbitmq-diagnostics".into(), "status".into()]),
                                }),
                                initial_delay_seconds: Some(30),
                                period_seconds: Some(60),
                                timeout_seconds: Some(15),
                                ..Default::default()
                            });
                            container.readiness_probe = Some(Probe {
                                exec: Some(ExecAction {
                                    command: Some(vec!["rabbitmq-diagnostics".into(), "ping".into()]),
                                }),
                                initial_delay_seconds: Some(20),
                                period_seconds: Some(60),
                                timeout_seconds: Some(10),
                                ..Default::default()
                            });

                            // done

                            Ok(())
                        })?;

                        Ok(())
                    })?;

                    spec.volume_claim_templates = Some(vec![PersistentVolumeClaim {
                        metadata: ObjectMeta {
                            labels: Some(selector.clone()),
                            name: Some("storage".into()),
                            ..Default::default()
                        },
                        spec: Some(PersistentVolumeClaimSpec {
                            access_modes: Some(vec!["ReadWriteOnce".into()]),
                            resources: Some(ResourceRequirements {
                                requests: Some(BTreeMap::from_iter(
                                    vec![("storage".to_string(), Quantity("8Gi".to_string()))]
                                        .into_iter(),
                                )),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }]);

                    Ok(())
                })?;

                Ok(statefulset)
            },
        )
            .await?;

        create_or_update(
            &self.services,
            resource.namespace(),
            format!("{}-headless", base),
            |mut svc| {
                svc.owned_by_controller(resource)?;
                svc.metadata.labels = Some(selector.clone());

                svc.spec.use_or_create_err(|spec| {
                    spec.selector = Some(selector.clone());
                    spec.type_ = Some("ClusterIP".into());
                    spec.cluster_ip = Some("None".into());

                    spec.ports = Some(vec![
                        ServicePort {
                            name: Some("amqp".into()),
                            port: 5672,
                            protocol: Some("TCP".into()),
                            target_port: Some(IntOrString::String("amqp".into())),
                            ..Default::default()
                        },
                        ServicePort {
                            name: Some("epmd".into()),
                            port: 4369,
                            protocol: Some("TCP".into()),
                            target_port: Some(IntOrString::String("epmd".into())),
                            ..Default::default()
                        },
                        ServicePort {
                            name: Some("cluster-links".into()),
                            port: 25672,
                            protocol: Some("TCP".into()),
                            target_port: Some(IntOrString::String("cluster-links".into())),
                            ..Default::default()
                        },
                    ]);

                    Ok(())
                })?;

                Ok(svc)
            },
        )
        .await?;

        create_or_update(&self.services, resource.namespace(), base, |mut svc| {
            svc.owned_by_controller(resource)?;
            svc.metadata.labels = Some(selector.clone());

            svc.spec.use_or_create_err(|spec| {
                spec.selector = Some(selector.clone());
                spec.type_ = Some("ClusterIP".into());

                spec.ports = Some(vec![
                    ServicePort {
                        name: Some("amqp".into()),
                        port: 5672,
                        protocol: Some("TCP".into()),
                        target_port: Some(IntOrString::String("amqp".into())),
                        ..Default::default()
                    },
                    ServicePort {
                        name: Some("management".into()),
                        port: 15672,
                        protocol: Some("TCP".into()),
                        target_port: Some(IntOrString::String("management".into())),
                        ..Default::default()
                    },
                ]);

                Ok(())
            })?;

            Ok(svc)
        })
        .await?;

        Ok(())
    }

    async fn delete_managed_rabbit(&self, resource: &Hawkbit) -> Result<()> {
        let base = format!("{}-rabbit", resource.name());
        let dp = DeleteParams {
            propagation_policy: Some(Background),
            ..Default::default()
        };

        self.service_accounts.delete_optionally(&base, &dp).await?;
        self.roles.delete_optionally(&base, &dp).await?;
        self.role_bindings.delete_optionally(&base, &dp).await?;
        self.configmaps.delete_optionally(&base, &dp).await?;
        self.secrets.delete_optionally(&base, &dp).await?;
        self.statefulsets.delete_optionally(&base, &dp).await?;
        self.services.delete_optionally(&base, &dp).await?;
        self.services
            .delete_optionally(&format!("{}-headless", &base), &dp)
            .await?;

        Ok(())
    }

    async fn create_application_config(
        &self,
        resource: &Hawkbit,
        namespace: &String,
        tracker: &mut ConfigTracker,
        issuer_uri: Option<String>,
    ) -> Result<()> {
        create_or_update(
            &self.configmaps,
            Some(namespace),
            resource.name(),
            |mut cm| {
                cm.owned_by_controller(resource)?;

                let mut cfg = r#"server:
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

                match &resource.spec.sign_on {
                    Some(SignOn::Keycloak { config: _ }) => {
                        cfg += &format!(
                            r#"
  security:
    oauth2:
      client:
        registration:
          hawkbit:
            provider: hawkbit-keycloak
            scope: openid
        provider:
          hawkbit-keycloak:
            issuer-uri: "{issuer_uri}"
"#,
                            issuer_uri = issuer_uri.unwrap_or_default()
                        );
                    }
                    None => {}
                }

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

                        match &resource.spec.sign_on {
                            Some(SignOn::Keycloak { config:_ }) => {
                                let secret_name = format!("keycloak-client-secret-{}-hawkbit", resource.name());
                                container.add_env_from_secret("SPRING_SECURITY_OAUTH2_CLIENT_REGISTRATION_HAWKBIT_CLIENTID", &secret_name, "CLIENT_ID")?;
                                container.add_env_from_secret("SPRING_SECURITY_OAUTH2_CLIENT_REGISTRATION_HAWKBIT_CLIENTSECRET", &secret_name, "CLIENT_SECRET")?;
                            },
                            None => {
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
                            }
                        }

                        match (
                            &resource.spec.rabbit.external,
                            &resource.spec.rabbit.managed,
                        ) {
                            (Some(external), None) => {
                                container.add_env("SPRING_RABBITMQ_HOST", &external.host)?;
                                container.add_env(
                                    "SPRING_RABBITMQ_PORT",
                                    format!("{}", external.port),
                                )?;
                                container
                                    .add_env("SPRING_RABBITMQ_USERNAME", &external.username)?;
                                container.add_env_from_secret(
                                    "SPRING_RABBITMQ_PASSWORD",
                                    &external.password_secret.name,
                                    &external.password_secret.field,
                                )?;

                                Ok(())
                            }
                            (None, Some(_)) => {
                                let rabbit = format!("{}-rabbit", resource.name());
                                container.add_env(
                                    "SPRING_RABBITMQ_HOST",
                                    format!("{}-rabbit", resource.name()),
                                )?;
                                container.add_env("SPRING_RABBITMQ_PORT", "5672")?;
                                container.add_env_from_secret(
                                    "SPRING_RABBITMQ_USERNAME",
                                    &rabbit,
                                    "default.username",
                                )?;
                                container.add_env_from_secret(
                                    "SPRING_RABBITMQ_PASSWORD",
                                    &rabbit,
                                    "default.password",
                                )?;

                                Ok(())
                            }
                            _ => Err(anyhow!("Invalid Rabbit configuration")),
                        }?;

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
    ) -> Result<Option<String>> {
        let selector = self.selector(resource, "server", "server");

        let mut url: Option<String> = None;

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

            let host = route.spec.host.clone();
            if !host.is_empty() {
                url = Some(format!("https://{}", host));
            }

            Ok(route)
        })
        .await?;

        Ok(url)
    }
}
