# Changelog

All notable changes to wherebox-cli. Generated from conventional commits by
`just release` (git-cliff, see cliff.toml).

## 0.2.0 - 2026-10-27

### Features

- Computed shelf life for perishables, estimated on hand for consumables (**breaking**)
- **matching:** Skip cuts and forms when finding the head noun
- **lists:** Food lists, head-noun scoring, densities and piece weights
- **stores:** Schema v4 with stores, chain layouts, store products and lot vendors
- **stores:** Store and source commands, profile stores, section guess and layouts
- **lists:** Source missing entries at the profile's store; list shop walks the layout
- **export:** Stores, chain layouts and store products round-trip through JSON

### Bug fixes

- **matching:** Food forms are the head noun wherever they sit
- **stores:** Import store rows by store_id; align the walk's aisle column

### Documentation

- Install options and the release process
- **spec:** Computed shelf life for perishables, estimated on-hand for consumables
- Shelf-life types, computed expiry and estimated on hand
- **spec:** Food lists, head-noun name matching, densities and piece weights
- Food lists and recipe matching in the README and JSON schema
- **spec:** Food forms as head nouns
- **spec:** Stores, store products, sections and the shopping route
- Stores, store products and the shopping route in the README and JSON schema

### Tests

- **recipes:** Flavours of sauces and cereals are not the food
- **stores:** Two chains, layout routes, section guesses and JSON round-trip

### Build and CI

- Run fmt, clippy and tests on master for Linux and macOS
- **release:** Package man pages and completions, check the tag, update the tap
- Mirror master and tags to GitLab when a deploy key is set
- Add just release for one-command releases
- Drop GitHub Actions; releases build and publish locally via just release (**breaking**)

## 0.1.0 - 2026-10-26

Initial release.
