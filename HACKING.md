## Linting helm chart

In Bash:

    docker run -v $(pwd):/home:z -ti --rm quay.io/helmpack/chart-testing sh -c "cd /home && ct lint --config .github/ct.yaml"

In Fish:

    docker run -v (pwd):/home:z -ti --rm quay.io/helmpack/chart-testing sh -c "cd /home && ct lint --config .github/ct.yaml"

## Validate OLM manifest

    operator-courier verify --ui_validate_io olm/hawkbit-operator-bundle/
