# tolly

Build this Petal with `petal build` and validate the resulting package with
`bloom petals build .`.

Before pushing, enforce the route architecture rules from the
`bloom-petal-development` skill:

```sh
bash scripts/check-route-architecture.sh
petal check
```
