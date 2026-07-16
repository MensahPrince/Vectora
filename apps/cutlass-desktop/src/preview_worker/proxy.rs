use super::*;

/// One current media descriptor captured before proxy refresh mutates the
/// engine's renderer state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProxyRefreshMedia {
    pub(super) id: MediaId,
    pub(super) path: PathBuf,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) is_video: bool,
}

/// Snapshot the media catalog needed to clear and re-request proxy bindings.
///
/// Project media lives in a hash map, so sorting makes the refresh order
/// deterministic without changing its semantics.
pub(super) fn plan_proxy_refresh(project: &Project) -> Vec<ProxyRefreshMedia> {
    let mut media: Vec<_> = project
        .media_iter()
        .map(|source| ProxyRefreshMedia {
            id: source.id,
            path: source.path().to_path_buf(),
            width: source.width,
            height: source.height,
            is_video: source.kind() == cutlass_models::MediaKind::Video,
        })
        .collect();
    media.sort_unstable_by_key(|source| source.id.raw());
    media
}

/// Rebind proxy-dependent runtime state after the proxy cache root moves.
///
/// This deliberately bypasses project commands: renderer substitutions,
/// strip scratch state, and delivered preview frames are session caches, so
/// refreshing them must not touch Project, history, or revision.
pub(super) fn refresh_proxies_after_maintenance(
    engine: &mut Engine,
    cache: &FrameCache,
    ui: &UiSink,
) {
    // Collect every descriptor before the first mutable Engine call so no
    // project borrow can overlap renderer mutation.
    let media = plan_proxy_refresh(engine.project());

    for source in &media {
        engine.clear_media_proxy(source.id);
    }
    ui.strips.clear_proxies();
    cache.clear();

    for source in media.into_iter().filter(|source| source.is_video) {
        ui.proxy
            .request(source.id.raw(), source.path, source.width, source.height);
    }
}

/// Bind a finished preview proxy to its pool media — only while the pool
/// entry still names `source`, the file the job was keyed to (a relink or
/// session swap in flight makes the id stale; the registries the engine
/// clears on those paths must never be repopulated with old files). On a
/// match the engine decodes the proxy from the next frame; delivered
/// frames composited from the original are dropped so the repaint (owed
/// via [`mutation_redraws_preview`]) and everything after render through
/// the proxy, and the strip worker re-points future filmstrip decodes.
pub(super) fn bind_media_proxy(
    engine: &mut Engine,
    media_id: u64,
    source: &Path,
    proxy: PathBuf,
    cache: &FrameCache,
    ui: &UiSink,
) {
    let media = MediaId::from_raw(media_id);
    match engine.project().media(media) {
        Some(m) if m.path() == source => {
            info!(%media, proxy = %proxy.display(), "preview proxy bound");
            engine.set_media_proxy(media, proxy.clone());
            cache.clear();
            ui.strips.register_proxy(media_id, proxy);
        }
        Some(_) => info!(%media, "proxy ignored: media was relinked while it generated"),
        None => info!(%media, "proxy ignored: media left the pool while it generated"),
    }
}
