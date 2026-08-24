# Contributing

## GitHub Actions

Pin every remote GitHub Action to a verified, full 40-character commit SHA from
the action's official repository. Add an adjacent comment with the corresponding
semantic version so that the pinned action remains recognizable and maintainable.

Local actions, such as `uses: ./path`, do not require commit SHA pins. Treat
Docker-based actions according to their image-pinning policy rather than as Git
commit references.
