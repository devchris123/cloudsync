pub enum Transaction<'tx> {
    #[cfg(test)]
    Read(&'tx redb::ReadTransaction),
    Write(&'tx redb::WriteTransaction),
}
