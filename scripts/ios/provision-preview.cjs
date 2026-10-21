#!/usr/bin/env node
// One-time local provisioning using the already authenticated EAS CLI.
// Check only by default; --apply signs into Apple and stores credentials in GitHub.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createRequire } = require('node:module');
const { execFileSync, spawnSync } = require('node:child_process');

const apply = process.argv.includes('--apply');
const easRoot = process.env.EAS_CLI_ROOT || path.dirname(path.dirname(
  fs.realpathSync(execFileSync('which', ['eas'], { encoding: 'utf8' }).trim())));
const fromEas = createRequire(path.join(easRoot, 'package.json'));
const { Auth, BundleId, Certificate, Device, Profile, ProfileType, CapabilityType,
  CapabilityTypeOption } = fromEas('@expo/apple-utils');
const { createGraphqlClient } = require(path.join(easRoot, 'build/commandUtils/context/contextUtils/createGraphqlClient'));
const { AppleDistributionCertificateQuery } = require(path.join(easRoot, 'build/credentials/ios/api/graphql/queries/AppleDistributionCertificateQuery'));
const { authenticateAsync } = require(path.join(easRoot, 'build/credentials/ios/appstore/authenticate'));

const repo = 'IIRoan/zeron';
const team = process.env.IOS_TEAM_ID || 'HXJACWP87H';
const bundle = process.env.IOS_BUNDLE_ID || 'com.iiroan.zeron';

function gh(args, input) {
  const result = spawnSync('gh', [...args, '--repo', repo], { input, encoding: 'utf8' });
  if (result.status !== 0) throw new Error('GitHub credential configuration failed; check gh auth status.');
}

async function main() {
  const auth = JSON.parse(fs.readFileSync(path.join(os.homedir(), '.expo/state.json'), 'utf8')).auth;
  const client = createGraphqlClient({ accessToken: process.env.EXPO_TOKEN || null, sessionSecret: auth.sessionSecret });
  const certificates = await AppleDistributionCertificateQuery.getAllForAccountAsync(client, 'iiroan');
  const certificate = certificates
    .filter(c => c.appleTeam?.appleTeamIdentifier === team && Date.parse(c.validityNotAfter) > Date.now())
    .sort((a, b) => Date.parse(b.validityNotAfter) - Date.parse(a.validityNotAfter))[0];
  if (!certificate) throw new Error('No valid EAS distribution certificate exists for this Apple team.');

  let context;
  if (apply) {
    // EAS handles the local password/2FA prompts. Neither is copied into GitHub.
    context = (await authenticateAsync({ teamId: team })).authState.context;
  } else {
    const { username } = JSON.parse(fs.readFileSync(path.join(os.homedir(), '.app-store/auth/username.json'), 'utf8'));
    const session = await Auth.tryRestoringAuthStateFromUserCredentialsAsync(
      { username, teamId: team }, { autoResolveProvider: true });
    if (!session) throw new Error('Apple login expired. Run this command locally with --apply to sign in and provision Zeron.');
    context = session.context;
  }
  if (context.teamId !== team) throw new Error('Apple session selected another team.');
  const appleCertificate = await Certificate.infoAsync(context, { id: certificate.developerPortalIdentifier });
  if (appleCertificate.attributes.serialNumber.toUpperCase() !== certificate.serialNumber.toUpperCase()) {
    throw new Error('EAS and Apple distribution certificate identities differ.');
  }
  const devices = (await Device.getAllIOSProfileDevicesAsync(context)).filter(
    d => d.attributes.status === 'ENABLED' && ['IPHONE', 'IPAD', 'IPOD'].includes(d.attributes.deviceClass));
  if (!devices.length) throw new Error('No enabled iOS devices are registered with Apple.');
  console.log(`Apple team ${team}: ${devices.length} enabled iPhone/iPad devices.`);
  for (const device of devices) console.log(`  ${device.attributes.name} (${device.attributes.model})`);
  const deviceIds = new Set(devices.map(d => d.id));
  const secrets = {};
  for (const [identifier, name, secret] of [
    [bundle, 'Zeron', 'IOS_APP_PROFILE'],
    [bundle + '.LiveActivity', 'Zeron Live Activity', 'IOS_ACTIVITY_PROFILE'],
  ]) {
    let app = await BundleId.findAsync(context, { identifier });
    if (!app && apply) app = await BundleId.createAsync(context, { identifier, name, platform: 'IOS' });
    if (!app) {
      console.log(`Would register ${identifier} and create its ad hoc profile.`);
      continue;
    }
    if (identifier === bundle && !(await app.hasCapabilityAsync(CapabilityType.PUSH_NOTIFICATIONS))) {
      if (apply) await app.updateBundleIdCapabilityAsync({
        capabilityType: CapabilityType.PUSH_NOTIFICATIONS, option: CapabilityTypeOption.ON,
      });
      else console.log(`Would enable the existing app's push entitlement for ${identifier}.`);
    }
    const profiles = await app.getProfilesAsync();
    let matching;
    for (const profile of profiles) {
      if (profile.attributes.profileType !== ProfileType.IOS_APP_ADHOC
          || profile.attributes.profileState !== 'ACTIVE'
          || Date.parse(profile.attributes.expirationDate) <= Date.now()) continue;
      const certs = await profile.getCertificatesAsync();
      const included = await profile.getDevicesAsync();
      if (certs.some(c => c.id === appleCertificate.id)
          && included.length === deviceIds.size && included.every(d => deviceIds.has(d.id))) {
        matching = profile;
        break;
      }
    }
    if (!matching && apply) matching = await Profile.createAsync(context, {
      bundleId: app.id, certificates: [appleCertificate.id], devices: [...deviceIds],
      name: `${name} Preview ${Date.now()}`, profileType: ProfileType.IOS_APP_ADHOC,
    });
    if (matching && !matching.attributes.profileContent) {
      matching = await Profile.infoAsync(context, { id: matching.id });
    }
    if (apply) {
      if (!matching?.attributes.profileContent) throw new Error(`No downloadable profile for ${identifier}.`);
      secrets[secret] = matching.attributes.profileContent;
    }
    console.log(`${identifier}: ${matching ? 'matching ad hoc profile ready' : 'would create ad hoc profile'}`);
  }
  if (!apply) return;
  secrets.IOS_DISTRIBUTION_P12 = certificate.certificateP12;
  secrets.IOS_DISTRIBUTION_PASSWORD = certificate.certificatePassword;
  for (const [name, value] of Object.entries(secrets)) {
    gh(['secret', 'set', name], value);
    console.log(`Stored ${name} in GitHub Secrets.`);
  }
  gh(['variable', 'set', 'IOS_TEAM_ID', '--body', team]);
  gh(['variable', 'set', 'IOS_BUNDLE_ID', '--body', bundle]);
  console.log('Preview credentials are ready. Run Direct instance iOS with distribution=preview.');
}

main().catch(error => { console.error(error.message); process.exitCode = 1; });
