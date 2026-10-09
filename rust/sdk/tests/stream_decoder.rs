use axiom_extension_sdk::{abi::{Event,EventBatch,Handle,HandleKind,Value},Stream};
fn handle()->Handle{Handle{id:1,generation:1,kind:HandleKind::Stream}}
fn batch(seq:u64,values:Vec<Value>)->EventBatch{EventBatch{subscription:handle(),events:values.into_iter().enumerate().map(|(i,value)|Event{sequence:seq+i as u64,value}).collect(),dropped:0,terminal:false}}
#[test]
fn sequences_are_atomic_and_terminal_handles_cannot_reopen(){
 let stream=Stream::<String>::from_value(&Value::Handle(handle())).unwrap();
 assert!(stream.decode_batch(&batch(1,vec![Value::String("one".into()),Value::Signed(2)])).is_err());assert_eq!(stream.next_sequence(),1);
 assert_eq!(stream.decode_batch(&batch(1,vec![Value::String("one".into())])).unwrap(),vec!["one"]);assert_eq!(stream.next_sequence(),2);
 for seq in [1,3]{assert!(stream.decode_batch(&batch(seq,vec![Value::String("wrong".into())])).is_err());assert_eq!(stream.next_sequence(),2);}
 let mut terminal=batch(2,vec![]);terminal.terminal=true;stream.decode_batch(&terminal).unwrap();assert!(stream.is_terminal());assert!(stream.decode_batch(&terminal).is_err());
}
#[test]
fn generation_loss_item_and_byte_bounds_are_checked_before_progress(){
 let stream=Stream::<String>::bounded(&Value::Handle(handle()),2,128).unwrap();
 let mut wrong=batch(1,vec![Value::String("one".into())]);wrong.subscription.generation=2;assert!(stream.decode_batch(&wrong).is_err());
 assert!(stream.decode_batch(&batch(1,vec![Value::String("huge".repeat(128))])).is_err());
 assert!(stream.decode_batch(&batch(1,vec![Value::String("x".into());3])).is_err());
 let mut loss=batch(1,vec![Value::String("latest".into())]);loss.dropped=3;assert!(stream.decode_batch(&loss).is_err());assert_eq!(stream.next_sequence(),1);
 let decoded=stream.decode_state_batch(&loss).unwrap();assert_eq!(decoded.dropped,3);assert_eq!(decoded.events,vec!["latest"]);assert_eq!(stream.next_sequence(),2);
}
