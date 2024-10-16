module Wid
  module Nodes
    class ClassDeclaration < Node
      attr_reader :name, :params, :body

      def initialize(name, superclass, body)
        @name = name
        @superclass = superclass
        @body = body
      end

      def children
        [@name, @superclass, @body]
      end

      def to_hash
        {class: self.class, name: @name.to_hash, superclass: @superclass&.to_hash, body: @body.to_hash}
      end
    end
  end
end
