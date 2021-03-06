/*
 * Copyright 2019 Balaji Jinnah and Contributors
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
use crate::config::config::Config;
use crate::partition::iterator::Iterator;
use crate::partition::posting_list;
use crate::store::batch::Batch;
use crate::store::store::Store;
use crate::types::types::{POSTING_LIST_ALL, SEGMENT_PREFIX};
use crate::util::decode_u64;
use failure;
use failure::bail;
use fst::{IntoStreamer, Set, Streamer};
use fst_levenshtein::Levenshtein;
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::marker::PhantomData;
use std::path;
use std::rc::Rc;
use std::time::Duration;
#[derive(Debug)]
pub struct Entry {
    pub line: Vec<u8>,
    pub ts: u64,
}

// SegmentIterator is used to iterate over segment files.
pub struct SegmentIterator<S> {
    store: S,
    pub entries: Vec<Rc<Entry>>,
    id: u64,
    pub current_index: usize,
    nothing_track: bool,
    partition: String,
}

impl<S: Store> SegmentIterator<S> {
    pub fn new(
        id: u64,
        partition_path: path::PathBuf,
        store: S,
        query: String,
        partition: String,
        start_ts: u64,
        end_ts: u64,
    ) -> Result<SegmentIterator<S>, failure::Error> {
        // let collect all the posting list for the given indices.
        let mut entry_indices = Vec::new();

        // query fst if there is any query string.
        if query != "" {
            // open index file.
            // no clue why it is unsafe by default.
            // TODO: investigate why it is unsafe and handle it properly.
            let index_set = unsafe {
                Set::from_path(partition_path.join(format!("segment_index_{}.fst", id)))
            }?;

            let fuzzy_query = Levenshtein::new(&query, 2)?;
            let indices_stream = index_set.search(fuzzy_query).into_stream().into_strs()?;
            // get all the posting list for the given indices.
            for index in indices_stream {
                // TODO: Don't use get here. Change this to prefix iterator. based on the key do the
                // decoding stuff.
                let index_key = format!("{}_{}_{}_{}", SEGMENT_PREFIX, partition, id, &index);
                let list = store.get(index_key.as_bytes())?;
                // skip if there is no value. Ideally we should thrown an error. Because index is
                // missing
                if list.is_none() {
                    panic!("posting list not found for the index  key {}", index_key);
                }
                let mut list = posting_list::decode_posting_list(&list.unwrap())?;
                entry_indices.append(&mut list);
            }
        } else {
            let index_key = format!(
                "{}_{}_{}_{}",
                SEGMENT_PREFIX, partition, id, POSTING_LIST_ALL
            );
            let list = store.get(index_key.as_bytes())?;
            // skip if there is no value. Ideally we should thrown an error. Because index is
            // missing
            if list.is_none() {
                panic!("posting list not found for the index  key {}", index_key);
            }
            let mut list = posting_list::decode_posting_list(&list.unwrap())?;
            entry_indices.append(&mut list);
        }
        // now sort all the indices. because there may be duplication and ordering.
        // But the indices in the posting list are in sorted order. Here we can make one
        // more micro optimization if there is only one set no need to sort.
        entry_indices.sort();
        // open segment file.
        let mut segment_file = File::open(partition_path.join(format!("{}.segment", id)))?;
        // we'll read all since random access is expensive.
        let mut buffer = Vec::new();
        segment_file.read_to_end(&mut buffer)?;
        // There may be lot of duplicate element so we should avoid that.
        let mut seen_set = HashSet::new();
        let mut entries = Vec::new();
        for line_offset in entry_indices {
            let mut read_offset = line_offset as usize;
            match seen_set.get(&read_offset) {
                Some(_) => {
                    // We already seen this offset so skipping this offset.
                    continue;
                }
                None => {
                    // Now we have to decode the entry.
                    // 8 bytes from this offset gives the length of the entry.
                    let entry_len = decode_u64(&buffer[read_offset..read_offset + 8]) as usize;
                    // advancing read offset.
                    read_offset = read_offset + 8;
                    let entry = decode_entry(&buffer[read_offset..read_offset + entry_len]);
                    if (start_ts <= entry.ts && entry.ts <= end_ts)
                        || (start_ts == 0 && end_ts == 0)
                    {
                        entries.push(Rc::new(entry));
                    }
                }
            }
            seen_set.insert(read_offset);
        }
        Ok(SegmentIterator {
            store: store,
            entries: entries,
            id: id,
            current_index: 0,
            nothing_track: true,
            partition: partition,
        })
    }
}

impl<S: Store> Iterator for SegmentIterator<S> {
    /// entry gives the iterators current entry.
    fn entry(&self) -> Option<Rc<Entry>> {
        let entry = self.entries.get(self.current_index);
        match entry {
            Some(ent) => Some(ent.clone()),
            None => None,
        }
    }

    /// next will advance the iterator. throws error if we reach end.
    fn next(&mut self) -> Option<()> {
        if self.current_index >= self.entries.len() - 1 {
            // just incrementing one so that entry will give none.
            self.current_index = self.current_index + 1;
            return None;
        }
        self.current_index = self.current_index + 1;
        Some(())
    }
}
pub fn decode_entry(line_buf: &[u8]) -> Entry {
    // first 8 bytes are timestamp.
    let ts = decode_u64(&line_buf[..8]);
    Entry {
        ts: ts,
        line: line_buf[8..].to_vec(),
    }
}
