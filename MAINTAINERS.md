# Maintainers

| Maintainer | GitHub | Role |
|------------|--------|------|
| Joao Henrique Machado Silva | [@joaoh82](https://github.com/joaoh82) | Author, lead maintainer |

## Scope

Maintainers review and merge changes, cut releases (see `.github/workflows/release.yml`),
and steward the design invariants in [`CLAUDE.md`](CLAUDE.md) and the specs under
[`spec/`](spec/).

## Contributing

Open a pull request. CI must be green (`make fmt-check`, `make clippy`, `make test`) and any
change to observable behavior must update the matching `examples/`, `tests/`, and `docs/`.
For larger work, see the roadmap in the project tracker and the milestone notes in the specs.
