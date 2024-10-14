module Wid
  module Nodes
    class EOFStatement < Expression
      def ==(other) = self.class == other&.class

      def children = []

      def to_hash = {class: self.class}
    end
  end
end
