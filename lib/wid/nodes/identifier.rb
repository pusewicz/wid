module Wid
  module Nodes
    class Identifier < Expression
      alias_method :name, :value

      def ==(other)
        name == other&.name
      end

      def children
        []
      end

      def to_hash
        {class: self.class, name: name}
      end
    end
  end
end
