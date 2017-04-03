crates-ectype
=====

crates-ectype (because there is already more than one crates-mirror, and I've read far too much Kant recently) is a basic Rust program made to essentially just clone the [crates.io-index](https://github.com/rust-lang/crates.io-index) repository, and then download every .crate file listed in the index. It also allows you to put a replacement URL, so that you can easily serve the mirror.

It is run simply as `crates-ectype /path/to/place/.crates/in`. You can optionally pass `--yanked` to also download yanked .crates, `--no-update-index` to not update the crates.io-index, and `--no-check-sums` to skip verifying the sha256sums of already downloaded .crates.

Replacement URLs are defined with `--replace=URL`. The URL should be the base URL for where clients can download the crates from, e.g. `https://crates.io/api/v1/crates`. Clients then use your mirror by pointing their cargo config to your index repository.

Beware that there are many crates, and it may take a while to download them all the first time around, and expect them to use at least 7GB of space.

## Example: Setting up a mirror with nginx and fcgiwrap

Let's say you want to host a mirror with nginx, and you want to store the downloaded crates in /srv/crates. You would first run `crates-ectype /srv/crates` to download all the crates.

Let's say you want to host the crates themselves under http://localhost/crates, and the index under http://localhost/crates.io-index. We'd need the following nginx directive to allow for downloading the .crate files (crates are downloaded as example.com/crates/*cratename*/*version*/download, where example.com/crates would be the replacement URL)
```
location /crates/ {
	alias /srv/crates/;
	rewrite (\S+)/(\S+)/download$ $1-$2.crate;
}
```
Next we need to make the index repository (which is a git repository) readable. There are many ways to do this, one way is with nginx, fcgiwrap and git-http-backend. Make sure those are installed (git-http-backend is likely at /usr/lib/git-core/git-http-backend) You'll want to start fcgiwrap (`systemctl start fcgiwrap.socket` depending on distro), and then use something like
```
location ~ /crates.io-index(/.*) {
	fastcgi_pass  unix:/var/run/fcgiwrap.sock;
	include       fastcgi_params;
	fastcgi_param SCRIPT_FILENAME     /usr/lib/git-core/git-http-backend;
	fastcgi_param GIT_HTTP_EXPORT_ALL "";
	fastcgi_param GIT_PROJECT_ROOT    /srv/crates/index;
	fastcgi_param PATH_INFO           $1;
}
```
Now we're almost done, we just need to update the URL in the index repository, do this by running `crates-ectype /srv/crates --replace=http://localhost/crates`. --replace replaces the DL option in the index config.json with the specified URL, which is the URL clients try to download the .crates from. If you want it to go faster you can add the --no-check-sums and --no-update-index options.

Now the mirror should ready. To use it, you'll just need to put the following in your ~/.cargo/config
```
[source.crates-io]
registry = "https://github.com/rust-lang/crates.io-index"
replace-with = 'my-mirror'
[source.my-mirror]
registry = "http://localhost/crates.io-index/"
```
([Source replacement docs](http://doc.crates.io/source-replacement.html))

To keep it up to date, you can create a cronjob to run crates-ectype. Just be sure to also include the --replace option, because every time you run crates-ectype without --no-update-index, the config.json is replaced with the original one.
