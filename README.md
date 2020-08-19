## Install the operator

You need to install the operator. Once you have installed the operator, you can create a new Hawkbit instance by
create a new custom resource of type `Hawkbit`.

### Using OperatorHub

The operator is available on [OperatorHub](https://operatorhub.io/operator/hawkbit-operator).

### Using Helm

You can also install the operator using [Helm](https://helm.sh/):

    helm install hawkbit-operator ./helm/hawkbit-operator

On OpenShift you can also build a local instance using S2I:

    helm install hawkbit-operator ./helm/hawkbit-operator --set s2i.enabled=true

## Create Hawkbit instance

* Create a new MySQL instance:

  ~~~
  helm install hawkbit-db bitnami/mysql --set master.securityContext.enabled=false --set db.name=hawkbit --set db.user=hawkbit --set db.password=hawkbit --set replication.enabled=false
  ~~~

* Create a new RabbitMQ instance:

  ~~~
  helm install hawkbit-rabbit bitnami/rabbitmq --set podSecurityContext= --set auth.username=hawkbit --set auth.password=hawkbit
  ~~~
 
* Create a new Hawkbit instance:

  Also see: [examples/](examples/)

  ~~~yaml
  kind: Hawkbit
  apiVersion: iot.eclipse.org/v1alpha1
  metadata:
    name: default
  spec:
    database:
      mysql:
        database: hawkbit
        host: hawkbit-db-mysql
        username: hawkbit
        passwordSecret:
          name: hawkbit-db-mysql
          field: mysql-password
    rabbit:
      host: hawkbit-rabbit-rabbitmq
      username: hawkbit
      passwordSecret:
        name: hawkbit-rabbit-rabbitmq
        field: rabbitmq-password
  ~~~

* Extract the admin credentials

  The operator will automatically create an initial admin user. The credentials can be extracted
  using the following commands:
  
  ~~~sh
  kubectl get secret default-admin -o jsonpath='{.data.adminUsername}' | base64 -d
  kubectl get secret default-admin -o jsonpath='{.data.adminPassword}' | base64 -d | cut -c7-
  ~~~
  
  You can update the secret with your own credentials and the operator will reconcile the deployment.
