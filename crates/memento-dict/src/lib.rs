//! Memento dictionary's pure lookup and stylesheet cores.

mod capi;
pub mod exactquerygenerator;
pub mod deconjugator;
pub mod deconjugationquerygenerator;
pub mod mecabquerygenerator;
pub mod yomidbbuilder;
pub mod databasemanager;
pub mod searchcontroller;
pub mod sqlite_db;

pub mod data {
    pub mod dictionarystyles_parser;
}
