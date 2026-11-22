//! Two bounded image workers; the clinical command loop never downloads or decodes pixels.
use gpui_kit::RenderImage;
use m53_client::connection::Connection;
use m53_contracts::workflow::Asset;
use m53_gpui_editor::Session;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};
use uuid::Uuid;

const MAX_THUMBNAILS: usize = 36;
const MAX_UPLOADS: usize = 4;
const TILE_WIDTH: f32 = 140.;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub full: Vec<Uuid>,
    pub thumbnails: Vec<Uuid>,
}
impl Selection {
    pub fn for_session(session: &Session, offset: f32, viewport: f32) -> Self {
        let visible: Vec<_> = session.photos.iter().filter(|p| !p.removed).collect();
        let selected = session.photos.get(session.selected).map(|p| p.id);
        let mut full = vec![];
        if let Some(index) = visible.iter().position(|p| Some(p.id) == selected) {
            full.push(visible[index].id);
            if index > 0 { full.push(visible[index - 1].id); }
            if index + 1 < visible.len() { full.push(visible[index + 1].id); }
        }
        let start = ((-offset.min(0.) / TILE_WIDTH).floor() as usize).saturating_sub(2);
        let count = ((viewport.max(TILE_WIDTH) / TILE_WIDTH).ceil() as usize + 5)
            .min(MAX_THUMBNAILS);
        Self {
            full,
            thumbnails: visible.iter().skip(start).take(count).map(|p| p.id).collect(),
        }
    }
    fn bounded(mut self) -> Self {
        let mut seen = BTreeSet::new();
        self.full.retain(|id| seen.insert(*id));
        self.full.truncate(3);
        seen.clear();
        self.thumbnails.retain(|id| seen.insert(*id));
        self.thumbnails.truncate(MAX_THUMBNAILS);
        self
    }
    #[cfg(test)]
    pub fn retain_textures<T>(&self, full: &mut std::collections::HashMap<Uuid,T>, thumbnails: &mut std::collections::HashMap<Uuid,T>) {
        full.retain(|id,_| self.full.contains(id));
        thumbnails.retain(|id,_| self.thumbnails.contains(id));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind { Full, Thumbnail }
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key { epoch: u64, study: Uuid, asset: Uuid, kind: Kind }
pub enum Source { Capture(Vec<u8>), Import(Vec<PathBuf>) }
struct Upload { batch: Uuid, study: Uuid, connection: Connection, source: Source }
pub enum Output {
    Full { study: Uuid, asset: Uuid, width: u32, height: u32, image: Arc<RenderImage> },
    Thumbnail { study: Uuid, asset: Uuid, image: Arc<RenderImage> },
    Added { batch: Uuid, study: Uuid, asset: Asset, width: u32, height: u32, captured: bool },
    Finished { batch: Uuid, error: Option<String> },
    Failed { study: Uuid, message: String },
}
enum Work { Load(Key, Connection), Upload(Upload) }
struct State {
    epoch: u64,
    context: Option<(String, Option<Uuid>, Uuid)>,
    connection: Option<Connection>,
    selection: Selection,
    queue: VecDeque<Key>,
    inflight: BTreeSet<Key>,
    ready: BTreeSet<Key>,
    failed: BTreeSet<Key>,
    retry: BTreeMap<Key, Instant>,
    uploads: VecDeque<Upload>,
    stopped: bool,
}
impl Default for State {
    fn default() -> Self {
        Self { epoch: 0, context: None, connection: None, selection: Selection::default(), queue: VecDeque::new(), inflight: BTreeSet::new(), ready: BTreeSet::new(), failed: BTreeSet::new(), retry: BTreeMap::new(), uploads: VecDeque::new(), stopped: false }
    }
}
impl State {
    fn keys(&self) -> Vec<Key> {
        let Some((_, _, study)) = self.context else { return vec![]; };
        let make = |asset, kind| Key { epoch: self.epoch, study, asset, kind };
        // First show the selected full image, then visible tiles, then full-size neighbors.
        self.selection.full.iter().take(1).map(|id| make(*id, Kind::Full))
            .chain(self.selection.thumbnails.iter().map(|id| make(*id, Kind::Thumbnail)))
            .chain(self.selection.full.iter().skip(1).map(|id| make(*id, Kind::Full)))
            .collect()
    }
    fn wanted(&self, key: Key) -> bool { self.keys().contains(&key) }
    fn fill(&mut self, now: Instant) {
        let wanted = self.keys();
        self.ready.retain(|key| wanted.contains(key));
        self.retry.retain(|key, _| wanted.contains(key));
        self.queue = wanted.into_iter().filter(|key| !self.inflight.contains(key)
            && !self.ready.contains(key) && !self.failed.contains(key)
            && self.retry.get(key).is_none_or(|deadline| *deadline <= now)
            && !(key.kind==Kind::Thumbnail && self.inflight.contains(&Key {kind:Kind::Full,..*key}))).collect();
    }
    fn clear(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.context = None;
        self.connection = None;
        self.selection = Selection::default();
        self.queue.clear(); self.ready.clear(); self.failed.clear(); self.retry.clear();
    }
    fn take(&mut self) -> Option<Work> {
        if let Some(upload) = self.uploads.pop_front() { return Some(Work::Upload(upload)); }
        self.fill(Instant::now());
        let key = self.queue.pop_front()?;
        let connection = self.connection.clone()?;
        self.inflight.insert(key);
        Some(Work::Load(key, connection))
    }
}
type Shared = Arc<(Mutex<State>, Condvar)>;
pub struct Loader { shared: Shared, pub incoming: mpsc::Receiver<Output> }
impl Loader {
    pub fn start() -> Self {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let (tx, incoming) = mpsc::sync_channel(4);
        for _ in 0..2 {
            let shared = shared.clone(); let tx = tx.clone();
            std::thread::spawn(move || worker(shared, tx));
        }
        Self { shared, incoming }
    }
    pub fn demand(&self, connection: Connection, study: Uuid, selection: Selection) {
        let (mutex, wake) = &*self.shared;
        let mut state = mutex.lock().unwrap();
        let context = (connection.base.clone(), connection.user_id, study);
        if state.context.as_ref() != Some(&context) { state.clear(); state.context = Some(context); }
        state.connection = Some(connection);
        state.selection = selection.bounded();
        state.fill(Instant::now()); wake.notify_all();
    }
    pub fn clear(&self) { let (mutex,wake)=&*self.shared; mutex.lock().unwrap().clear(); wake.notify_all(); }
    pub fn refresh(&self) {
        let (mutex,wake)=&*self.shared; let mut state=mutex.lock().unwrap();
        state.failed.clear(); state.ready.clear(); state.retry.clear(); state.fill(Instant::now()); wake.notify_all();
    }
    pub fn upload(&self, batch: Uuid, study: Uuid, connection: Connection, source: Source) -> Result<(), String> {
        let (mutex,wake)=&*self.shared; let mut state=mutex.lock().unwrap();
        if state.uploads.len() >= MAX_UPLOADS { return Err("Очередь импорта занята; дождитесь сохранения снимков".into()); }
        state.uploads.push_back(Upload { batch, study, connection, source }); wake.notify_one(); Ok(())
    }
}
impl Drop for Loader {
    fn drop(&mut self) { let (mutex,wake)=&*self.shared; mutex.lock().unwrap().stopped=true; wake.notify_all(); }
}
fn pending(error: &str) -> bool {
    ["еще не", "ещё не", "загруж", "Нет связи", "Сервис недоступен", "не в локальном кеше"]
        .iter().any(|part| error.contains(part))
}
fn worker(shared: Shared, tx: mpsc::SyncSender<Output>) {
    loop {
        let work = {
            let (mutex,wake)=&*shared; let mut state=mutex.lock().unwrap();
            loop {
                if state.stopped { return; }
                if let Some(work)=state.take() { break work; }
                state=wake.wait_timeout(state,Duration::from_millis(500)).unwrap().0;
            }
        };
        match work {
            Work::Upload(upload) => ingest(upload, &tx),
            Work::Load(key, connection) => {
                let result = match key.kind {
                    Kind::Full => connection.download_shared(key.asset).and_then(|bytes| crate::storage::previews(&bytes).map(|(w,h,full,thumb)|(w,h,full,Some(thumb)))),
                    Kind::Thumbnail => {
                        let bytes=match connection.thumbnail_shared(key.asset) {
                            Ok(bytes)=>Ok(bytes),
                            Err(error) if pending(&error)=>connection.cached_original_shared(key.asset).and_then(|bytes|bytes.ok_or(error)),
                            Err(error)=>Err(error),
                        };
                        bytes.and_then(|bytes| crate::storage::thumbnail(&bytes).map(|image|(0,0,image,None)))
                    },
                };
                let mut output = vec![];
                {
                    let mut state=shared.0.lock().unwrap(); state.inflight.remove(&key);
                    if !state.wanted(key) { continue; }
                    match result {
                        Ok((width,height,image,thumbnail)) => {
                            state.ready.insert(key); state.retry.remove(&key);
                            match key.kind {
                                Kind::Full => {
                                    output.push(Output::Full { study:key.study, asset:key.asset, width,height,image });
                                    let thumb_key=Key { kind:Kind::Thumbnail,..key };
                                    if state.wanted(thumb_key) && let Some(image)=thumbnail {
                                        state.ready.insert(thumb_key);
                                        output.push(Output::Thumbnail { study:key.study,asset:key.asset,image });
                                    }
                                }
                                Kind::Thumbnail => output.push(Output::Thumbnail {study:key.study,asset:key.asset,image}),
                            }
                        }
                        Err(error) if pending(&error) => {state.retry.insert(key,Instant::now()+Duration::from_millis(500));}
                        Err(error) => {state.failed.insert(key);output.push(Output::Failed {study:key.study,message:error});}
                    }
                }
                for event in output { if tx.send(event).is_err() { return; } }
            }
        }
    }
}
fn ingest(upload: Upload, tx: &mpsc::SyncSender<Output>) {
    let Upload {batch,study,connection,source}=upload;
    let captured=matches!(source,Source::Capture(_));
    let result=(||->Result<(),String> {
        let inputs:Box<dyn Iterator<Item=Result<Vec<u8>,String>>>=match source {Source::Capture(bytes)=>Box::new(std::iter::once(Ok(bytes))),Source::Import(paths)=>{
            Box::new(paths.into_iter().map(|path| {
                use std::io::Read;
                let mut bytes=vec![];
                std::fs::File::open(path).map_err(|_|"Не удалось открыть снимок".to_string())?
                    .take(m53_client::imaging::MAX_FILE_BYTES as u64+1).read_to_end(&mut bytes).map_err(|_|"Ошибка чтения снимка".to_string())?;
                Ok(bytes)
            }))
        }};
        for bytes in inputs {
            let asset=connection.upload(study,"original",bytes?)?;
            let bytes=connection.download_shared(asset.id)?;
            let (width,height)=m53_client::imaging::dimensions_from_bytes(&bytes)?;
            tx.send(Output::Added {batch,study,asset,width,height,captured}).map_err(|_|"Обработчик снимков остановлен".to_string())?;
        }
        Ok(())
    })();
    let _=tx.send(Output::Finished {batch,error:result.err()});
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(count:usize,selected:usize)->Session {
        Session { photos:(0..count).map(|_|m53_gpui_editor::Photo::new(4000,3000)).collect(),selected }
    }
    #[test]
    fn demand_only_keeps_selected_neighbors_and_visible_strip() {
        let mut session=session(1000,500); session.photos[499].removed=true;
        let demand=Selection::for_session(&session,-1400.,700.);
        assert_eq!(demand.full,vec![session.photos[500].id,session.photos[498].id,session.photos[501].id]);
        assert_eq!(demand.thumbnails.len(),10);
        assert_eq!(demand.thumbnails[0],session.photos[8].id);
        assert!(!demand.thumbnails.contains(&session.photos[500].id));
        assert_eq!((session.photos[500].width,session.photos[500].height),(4000,3000));
    }
    #[test]
    fn bounded_queue_deduplicates_and_drops_stale_requests() {
        let mut state=State::default();
        let study=Uuid::new_v4(); state.context=Some(("http://test".into(),None,study));
        let session=session(500,250);
        state.selection=Selection::for_session(&session,0.,100000.).bounded(); state.fill(Instant::now());
        assert!(state.queue.len()<=MAX_THUMBNAILS+3);
        let first=state.queue.pop_front().unwrap(); state.inflight.insert(first);
        state.fill(Instant::now()); assert!(!state.queue.contains(&first));
        state.selection=Selection::for_session(&session,-14000.,700.); state.fill(Instant::now());
        assert!(state.queue.iter().all(|key|state.wanted(*key)));
        state.clear(); assert!(!state.wanted(first)); assert!(state.queue.is_empty());
    }
    #[test]
    fn failures_wait_for_refresh_and_pending_retries_are_bounded() {
        let mut state=State::default(); state.context=Some(("http://test".into(),None,Uuid::new_v4()));
        state.selection=Selection::for_session(&session(10,0),0.,700.);
        let key=state.keys()[0]; let now=Instant::now();
        state.retry.insert(key,now+Duration::from_millis(500)); state.fill(now); assert!(!state.queue.contains(&key));
        state.fill(now+Duration::from_millis(501)); assert!(state.queue.contains(&key));
        state.failed.insert(key); state.fill(now+Duration::from_secs(1)); assert!(!state.queue.contains(&key));
        state.failed.clear(); state.fill(now+Duration::from_secs(1)); assert!(state.queue.contains(&key));
    }
    #[test]
    fn selection_evicts_pixel_textures_without_changing_original_resolution() {
        let mut session=session(1000,400);
        let mut full:std::collections::HashMap<_,_>=session.photos.iter().map(|p|(p.id,())).collect();
        let mut thumbnails=full.clone();
        let selection=Selection::for_session(&session,-2800.,980.);
        selection.retain_textures(&mut full,&mut thumbnails);
        assert_eq!(full.len(),3); assert!(thumbnails.len()<=MAX_THUMBNAILS);
        let stale=session.photos[400].id;
        session.selected=700;
        let resized=Selection::for_session(&session,-8400.,1960.);
        resized.retain_textures(&mut full,&mut thumbnails);
        assert!(!resized.full.contains(&stale)); assert!(full.is_empty());
        assert!(session.photos.iter().all(|p|(p.width,p.height)==(4000,3000)));
    }
}
