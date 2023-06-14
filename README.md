# Just One Init

For use with stateful apps that are running within Kubernetes. Uses the
leasing mechanism in Kubernetes limit the running instances of a pod to
1, while still having a pod almost ready to go.

## Usage

### Via Dockerfile

``` dockerfile
FROM ghcr.io/purplebooth/justoneinit:latest AS just-one-init
FROM ubuntu:latest

COPY --from=just-one-init /usr/local/bin/just-one-init /just-one-init

ENV LEASE_NAME="my-lease"
ENTRYPOINT ["/just-one-init", "--"]
CMD ["bash", "-c", "Hello World"]
```

Then add it to your deployment like so. Note that we allow the startup
and liveness probes pass if the init container is returning a 404. This
indicates that it does not hold the lease. The readiness probe ensure
that the container is not included in endpoints.

``` yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-app
spec:
  replicas: 2
  selector:
    matchLabels:
      app: my-app
  template:
    metadata:
      labels:
        app: my-app
    spec:
      serviceAccountName: just-one-init
      containers:
        - name: my-app
          image: my-app:latest
          readinessProbe:
            httpGet:
              port: 5047
              path: /healthcheck
          livenessProbe: &probe
            exec:
              command:
                - /bin/bash
                - -c
                - |
                  if [ "$(curl --write-out "%{http_code}\n" --silent --output /dev/null "http://127.0.0.1:5047")" -eq 404 ] || echo your healthcheck ; then
                    exit 0
                  else
                    exit 1
                  fi
          startupProbe: *probe
          env:
            - name: POD_NAMESPACE
              valueFrom:
                fieldRef:
                  apiVersion: v1
                  fieldPath: metadata.namespace
```

### Using an existing docker file

There are situations where you want to use an existing docker file. In
this case you can use a volume to get the binary into your docker image.

``` yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-app
spec:
  replicas: 2
  selector:
    matchLabels:
      app: my-app
  template:
    metadata:
      labels:
        app: my-app
    spec:
      serviceAccountName: just-one-init
      initContainers:
        - name: just-one-init
          image: ghcr.io/purplebooth/just-one-init:latest
          command:
            - cp
            - '-v'
            - /usr/local/bin/just-one-init
            - /just-one-init/just-one-init
          volumeMounts:
            - name: just-one-init
              mountPath: /just-one-init
      volumes:
        - name: just-one-init
          emptyDir: { }
      containers:
        - name: my-app
          image: my-app:latest
          env:
            - name: LEASE_NAME
              value: the-lease-name
            - name: POD_NAMESPACE
              valueFrom:
                fieldRef:
                  fieldPath: metadata.namespace
          command:
            - /just-one-init/just-one-init
            - --
            - whatever-the-original-entrypoint-was
          volumeMounts:
            - mountPath: /just-one-init
              name: just-one-init
          readinessProbe:
            httpGet:
              port: 5047
              path: /healthcheck
          livenessProbe: &probe
            exec:
              command:
                - /bin/bash
                - -c
                - |
                  if [ "$(curl --write-out "%{http_code}\n" --silent --output /dev/null "http://127.0.0.1:5047")" -eq 404 ] || echo your healthcheck ; then
                    exit 0
                  else
                    exit 1
                  fi
          startupProbe: *probe
```

If you are unsure what the original entrypoint was you can find it by
running

``` bash
docker pull my-app:latest
docker inspect my-app:latest | jq '.[0].Config.Entrypoint'
```

An example of the service account can be found in the
[k8s/service-account](k8s/service-account) directory.
