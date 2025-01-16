// Run a post-install check to see that a) we actually support the current
// platform and b) the native lib was installed.  There's nothing fancy about
// the check - we're just running `require` to make sure the library can be
// loaded.
//
// This script assums its run from an install into `node_modules` where the
// platform-specific binary has already been installed. In local dev, `npm
// install` or `npm ci` will try to install the package but the platform
// specific binaries won't be installed - there's no graceful way to avoid
// failing this check so if you set JUNCTION_CLIENT_SKIP_POSTINSTALL we'll
// just not run.
if (process.env.JUNCTION_CLIENT_SKIP_POSTINSTALL) {
  process.exit(0);
}

const { platform, arch } = process;

const PLATFORMS = {
  darwin: {
    arm64: "darwin-arm64",
    x64: "darwin-x64",
  },
  linux: {
    arm64: "linux-arm64-gnu",
    x64: "linux-x64-gnu",
  },
  win32: {
    x64: "win32-x64-msvc",
  },
};

const platformSuffix = PLATFORMS?.[platform]?.[arch];

if (!platform) {
  console.error(
    `No native library is available for your platform/cpu (${platform}/${arch}). Junction will not function!`,
  );
  process.exit(2);
}

const libName = `@junction-labs/client-${platformSuffix}/index.node`;
let path;
try {
  path = require.resolve(libName);
} catch {
  console.error(
    `Failed to install native lib ${libName} for @junction-labs/client. Junction will not function!`,
    "\n",
    "",
  );
  process.exit(3);
}
