# Showcase

Every screenshot is rendered from the synthetic data in
[`examples/`](../examples) in a throwaway HOME with a throwaway test key; no
real inventory is read.

Regenerate all of them (needs `wherebox` on `PATH` and ImageMagick):

```sh
docs/showcase/make-screenshots.sh
```

## tour.png

![tour](showcase/tour.png)

Reproduce by hand:

```sh
wherebox import examples/homebox.csv
wherebox loc ls
wherebox ls --columns name,location,value
```
