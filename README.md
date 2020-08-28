## Install the operator

You need to install the operator. Once you have installed the operator, you can create a new hawkBit instance by
create a new custom resource of type `Hawkbit`.

### Using OperatorHub

The operator is available on [OperatorHub](https://operatorhub.io/operator/hawkbit-operator).

### Using Helm

You can also install the operator using [Helm](https://helm.sh/):

    helm install hawkbit-operator ./helm/hawkbit-operator

On OpenShift you can also build a local instance using S2I:

    helm install hawkbit-operator ./helm/hawkbit-operator --set s2i.enabled=true

## Create a hawkBit instance

* Have an existing database instance, or choose the embedded one

  hawkBit requires a database to run. You can provide an existing database instance, or
  you can choose from the following options:

  * Use an embedded database:
  
    This can be achieved by using the following `database` configuration in the `Hawkbit` resource:
  
    ~~~yaml
    spec:
      database:
        embedded: {}
    ~~~
    
    NOTE: This uses an embedded instance of H2. This is only intended for testing.

  * Create a new PostgreSQL instance:
    
    NOTE: This currently requires a manually provide hawkBit image.
    
    ~~~
    helm install hawkbit-db bitnami/postgresql --set securityContext.enabled=false --set postgresqlDatabase=hawkbit --set postgresqlUsername=hawkbit --set postgresqlPassword=hawkbit
    ~~~
    
    And use the following `database` configuration in the `Hawkbit` resource:
    
    ~~~yaml
    spec:
      database:
        postgres:
          database: hawkbit
          host: hawkbit-db-postgresql
          username: hawkbit
          passwordSecret:
            name: hawkbit-db-postgresql
            field: postgresql-password
    ~~~

  * Create a new MySQL instance:

    ~~~
    helm install hawkbit-db bitnami/mysql --set master.securityContext.enabled=false --set db.name=hawkbit --set db.user=hawkbit --set db.password=hawkbit --set replication.enabled=false
    ~~~
    
    And use the following `database` configuration in the `Hawkbit` resource:
    
    ~~~yaml
    spec:
      database:
        mysql:
          database: hawkbit
          host: hawkbit-db-mysql
          username: hawkbit
          passwordSecret:
            name: hawkbit-db-mysql
            field: mysql-password
    ~~~

* Have an existing broker instance, or choose the managed one

  Eclipse hawkBit requires a RabbitMQ broker. You can use an existing instance or let the
  operator manage one for you.

    * Let the operator manage an instance:
    
      ~~~yaml
      spec:
        rabbit:
          managed: {}
      ~~~
    
    * Or create a new RabbitMQ instance:
      
      ~~~
      helm install hawkbit-rabbit bitnami/rabbitmq --set podSecurityContext= --set auth.username=hawkbit --set auth.password=hawkbit
      ~~~
      
      And use the following configuration:
      
      ~~~yaml
      spec:
        rabbit:
          external:
            host: hawkbit-rabbit-rabbitmq
            username: hawkbit
            passwordSecret:
              name: hawkbit-rabbit-rabbitmq
              field: rabbitmq-password
      ~~~
     
* Create a new hawkBit instance:

  Also see the snippets above or the other examples: [examples/](examples/).

  ~~~yaml
  kind: Hawkbit
  apiVersion: iot.eclipse.org/v1alpha1
  metadata:
    name: default
  spec:
    database:
      embedded: {}
    rabbit:
      managed: {}
  ~~~

* Extract the admin credentials

  The operator will automatically create an initial admin user. The credentials can be extracted
  using the following commands:
  
  ~~~sh
  kubectl get secret default-admin -o jsonpath='{.data.adminUsername}' | base64 -d
  kubectl get secret default-admin -o jsonpath='{.data.adminPassword}' | base64 -d | cut -c7-
  ~~~
  
  You can update the secret with your own credentials and the operator will reconcile the deployment.
