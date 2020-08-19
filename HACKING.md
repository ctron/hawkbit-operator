## Linting helm chart

    docker run -v (pwd):/home:z -ti --rm quay.io/helmpack/chart-testing sh -c "cd /home && ct lint --charts helm/ditto-operator/"

## Validate OLM manifest

    operator-courier verify --ui_validate_io olm/hawkbit-operator-bundle/
